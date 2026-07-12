//! Tests for cross-process buffer mapping (`SharedBuffer::map_into_process`,
//! the resource-layer primitive behind `OP_BUFFER_MAP`).
//!
//! These exercise the mapping-ownership/keepalive design directly: a
//! `SharedBuffer`'s physical frames must stay alive for as long as ANY
//! process still has them mapped, regardless of what happens to the
//! `Arc<SharedBuffer>` reference that was used to create the mapping (a
//! stand-in for "the allocating process's own handle"). See the
//! "Cross-process mapping safety" doc comment on `SharedBuffer` in
//! `panda-kernel/src/resource/buffer.rs` for the full reasoning.
//!
//! The syscall-level orchestration (`OP_BUFFER_MAP`'s handle resolution,
//! `InvalidHandle` on a non-buffer handle) isn't reachable from a
//! kernel-only integration test since it needs a running process/scheduler
//! — mirrors `handle_transfer.rs`'s split. The userspace test
//! `buffer_transfer_test` exercises that full syscall/ABI path end-to-end,
//! including the non-buffer-handle negative case.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::sync::Arc;
use alloc::vec::Vec;

use panda_kernel::memory;
use panda_kernel::process::{Process, context::Context};
use panda_kernel::resource::{BufferExt, SharedBuffer};

panda_kernel::test_harness!(
    second_mapping_keeps_frames_alive_after_allocating_reference_drops,
    frames_are_freed_once_every_mapping_and_reference_are_gone,
    mapping_same_buffer_twice_creates_independent_mappings,
);

/// Build a minimal (headerless-body) ELF process for use as a mapping
/// target. Mirrors `create_test_process` in `tests/resource.rs` — each
/// integration test file is a separate crate, so this can't be shared.
fn create_test_process() -> Process {
    let mut elf_data = alloc::vec![0u8; 4096];

    elf_data[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']); // magic
    elf_data[4] = 2; // 64-bit
    elf_data[5] = 1; // little endian
    elf_data[6] = 1; // version
    elf_data[16] = 2; // e_type = ET_EXEC
    elf_data[18] = 0x3e; // e_machine = x86-64
    elf_data[20] = 1; // e_version
    elf_data[24..32].copy_from_slice(&0x400000u64.to_le_bytes()); // e_entry
    elf_data[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
    elf_data[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
    elf_data[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
    elf_data[56..58].copy_from_slice(&0u16.to_le_bytes()); // e_phnum = 0

    let context = Context::new_user_context();
    let elf_slice: &[u8] = &elf_data;
    Process::from_elf_data(context, elf_slice as *const [u8])
        .expect("failed to create test process from ELF data")
}

/// Read `len` bytes from a user-mapped address in the CURRENTLY active
/// address space, bracketed for SMAP exactly like `BufferExt::with_slice`
/// does internally.
fn read_user_bytes(vaddr: usize, len: usize) -> Vec<u8> {
    memory::smap::with_userspace_access(|| unsafe {
        core::slice::from_raw_parts(vaddr as *const u8, len).to_vec()
    })
}

/// The central cross-process safety property: a second process's mapping
/// (`map_into_process`, `OP_BUFFER_MAP`'s kernel primitive) keeps a
/// buffer's frames alive even after the reference used to create the
/// buffer (standing in for the allocating process's own handle) is
/// dropped — the two-mappings-plus-drop-order scenario from the task.
fn second_mapping_keeps_frames_alive_after_allocating_reference_drops() {
    let original_page_table = memory::current_page_table_phys();

    let mut proc_a = create_test_process();
    let mut proc_b = create_test_process();

    // Allocate the buffer and write a pattern while A's page table is
    // active — SharedBuffer::alloc installs page-table entries in
    // whichever address space is currently active (the allocating
    // process's own, by construction of the syscall path).
    unsafe { memory::switch_page_table(proc_a.page_table_phys()) };
    let (buffer, _addr_a) =
        SharedBuffer::alloc(&mut proc_a, 4096).expect("alloc should succeed");
    buffer.with_mut_slice(|s| s.fill(0xAB));

    // A Weak lets us observe exactly when the buffer is actually freed,
    // without touching frame-allocator internals.
    let weak = Arc::downgrade(&buffer);

    // Map the SAME buffer into process B while B's page table is active —
    // this is the OP_BUFFER_MAP kernel primitive.
    unsafe { memory::switch_page_table(proc_b.page_table_phys()) };
    let addr_b = buffer
        .map_into_process(&mut proc_b)
        .expect("map_into_process should succeed");

    // B sees the same physical frames A wrote to.
    assert_eq!(
        read_user_bytes(addr_b, 4096),
        alloc::vec![0xABu8; 4096],
        "process B's mapping should observe process A's pattern (shared physical frames)"
    );

    // Drop the buffer's own Arc — this is the reference that stands in for
    // the allocating process's handle table entry (or, in the syscall
    // path, whatever's left of the allocating process once it exits: see
    // the "Handle close" / "Process exit" bullets in SharedBuffer's doc
    // comment). Process B's ExternalFrames mapping still holds a keepalive
    // clone, so the buffer must NOT be freed yet.
    drop(buffer);
    assert!(
        weak.upgrade().is_some(),
        "buffer must stay alive: process B's mapping still references it"
    );
    assert_eq!(
        read_user_bytes(addr_b, 4096),
        alloc::vec![0xABu8; 4096],
        "frames must still be valid and mapped in B after the allocating reference is dropped"
    );

    // "Process B exits": drop the Process while its OWN page table is
    // still active, exactly like the real OP_PROCESS_EXIT path (the
    // scheduler drops the exiting process before switching to the next
    // runnable one — see `scheduler::remove_process`). Dropping
    // `proc_b.mappings` drops the ExternalFrames mapping, which unmaps B's
    // page-table entries and then drops its keepalive Arc clone — the
    // last surviving strong reference.
    drop(proc_b);
    assert!(
        weak.upgrade().is_none(),
        "buffer must finally be freed once the last mapping (B's keepalive) is gone"
    );

    // Leave the environment clean for subsequent tests in this harness.
    unsafe { memory::switch_page_table(original_page_table) };
    drop(proc_a);
}

/// Simplest possible version of the keepalive property, isolating "frames
/// survive with zero direct `Arc<SharedBuffer>` references outstanding, as
/// long as one process's mapping still exists" from the read/write
/// plumbing above.
fn frames_are_freed_once_every_mapping_and_reference_are_gone() {
    let original_page_table = memory::current_page_table_phys();

    let mut proc_a = create_test_process();
    let mut proc_b = create_test_process();

    unsafe { memory::switch_page_table(proc_a.page_table_phys()) };
    let (buffer, _addr_a) =
        SharedBuffer::alloc(&mut proc_a, 4096).expect("alloc should succeed");
    let weak = Arc::downgrade(&buffer);

    unsafe { memory::switch_page_table(proc_b.page_table_phys()) };
    buffer
        .map_into_process(&mut proc_b)
        .expect("map_into_process should succeed");

    drop(buffer);

    unsafe { memory::switch_page_table(original_page_table) };
    drop(proc_a);
    // proc_a never held a mapping into this buffer directly (SharedBuffer's
    // own `_mapping` lives inside the SharedBuffer struct itself, not in
    // `Process::mappings`), so dropping proc_a has no effect on the
    // buffer's liveness either way.
    assert!(
        weak.upgrade().is_some(),
        "buffer must still be alive: process B's mapping is the sole remaining reference"
    );

    unsafe { memory::switch_page_table(proc_b.page_table_phys()) };
    drop(proc_b);
    assert!(
        weak.upgrade().is_none(),
        "buffer must be freed once process B (the last mapping) exits"
    );

    unsafe { memory::switch_page_table(original_page_table) };
}

/// Idempotence policy (documented on `SharedBuffer::map_into_process`):
/// mapping the same buffer more than once in the same process is NOT
/// deduplicated — each call creates an independent mapping at a fresh
/// vaddr range, including when called again in the allocating process
/// itself (which already has the buffer mapped from `alloc()`).
fn mapping_same_buffer_twice_creates_independent_mappings() {
    let original_page_table = memory::current_page_table_phys();

    let mut proc_a = create_test_process();
    unsafe { memory::switch_page_table(proc_a.page_table_phys()) };

    let (buffer, addr_alloc) =
        SharedBuffer::alloc(&mut proc_a, 4096).expect("alloc should succeed");
    buffer.with_mut_slice(|s| s.fill(0xCD));

    let addr_map_1 = buffer
        .map_into_process(&mut proc_a)
        .expect("first map_into_process should succeed");
    let addr_map_2 = buffer
        .map_into_process(&mut proc_a)
        .expect("second map_into_process should succeed");

    assert_ne!(
        addr_map_1, addr_map_2,
        "mapping the same buffer twice must yield two independent vaddr ranges"
    );
    assert_ne!(
        addr_alloc, addr_map_1,
        "OP_BUFFER_MAP in the allocating process must not just return the alloc-time address"
    );
    assert_ne!(
        addr_alloc, addr_map_2,
        "OP_BUFFER_MAP in the allocating process must not just return the alloc-time address"
    );

    // Both new mappings observe the same underlying frames as the original.
    assert_eq!(read_user_bytes(addr_map_1, 4096), alloc::vec![0xCDu8; 4096]);
    assert_eq!(read_user_bytes(addr_map_2, 4096), alloc::vec![0xCDu8; 4096]);

    unsafe { memory::switch_page_table(original_page_table) };
    drop(proc_a);
}
