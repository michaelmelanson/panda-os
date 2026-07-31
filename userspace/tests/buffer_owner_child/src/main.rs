//! Child half of buffer_owner_test: receives a transferred buffer handle
//! and verifies that allocator-context-dependent syscalls on it fail
//! cleanly instead of touching this process's unrelated memory.
//!
//! Layout trick that makes the bug observable: both processes' buffer
//! vaddr allocators start at the same base address. This child allocates
//! its OWN buffer first, which therefore occupies the same numeric address
//! range in this process that the transferred buffer occupies in the
//! parent. Any syscall that wrongly dereferences (or reclaims) the
//! parent's vaddr in THIS process actually hits the child's own buffer —
//! turning the cross-address-space confusion into a deterministic,
//! assertable corruption.

#![no_std]
#![no_main]

use libpanda::{buffer::Buffer, environment, ipc::Channel, sys};

const BUFFER_SIZE: usize = 4096;
const CHILD_MARKER: u8 = 0x43; // 'C'

libpanda::main! {
    environment::log("Buffer owner child: starting");

    // Occupy the low buffer vaddr range with our own buffer (see module doc).
    let Some(mut own) = Buffer::alloc(BUFFER_SIZE) else {
        environment::log("FAIL: own buffer alloc failed");
        return 1;
    };
    own.as_mut_slice().fill(CHILD_MARKER);

    let Some(parent) = Channel::parent() else {
        environment::log("FAIL: no parent channel");
        return 1;
    };
    let mut msg = [0u8; 64];
    let (_len, attached) = match parent.recv_with_handle(&mut msg) {
        Ok(r) => r,
        Err(_) => {
            environment::log("FAIL: recv_with_handle failed");
            return 1;
        }
    };
    let Some(transferred) = attached else {
        environment::log("FAIL: expected a transferred handle");
        return 1;
    };
    environment::log("Buffer owner child: received transferred buffer");

    // Test 1: OP_FILE_READ_BUFFER on a non-owned buffer must fail cleanly.
    // If it instead dereferences the allocator's vaddr in OUR address
    // space, the file contents land in `own` (which sits at that address
    // here) — corrupting memory the kernel was never asked to touch.
    let Ok(file) = environment::open("file:/initrd/buffer_owner_child", 0, 0) else {
        environment::log("FAIL: could not open our own binary from initrd");
        return 1;
    };
    let read_result = sys::buffer::read_from_file(file, transferred);
    if read_result >= 0 {
        environment::log("FAIL: read_from_file on non-owned buffer succeeded");
        return 1;
    }
    if own.as_slice().iter().any(|&b| b != CHILD_MARKER) {
        environment::log("FAIL: read_from_file corrupted our own buffer");
        return 1;
    }
    environment::log("Buffer owner child: read into non-owned buffer refused");

    // Test 2: OP_FILE_WRITE_BUFFER on a non-owned buffer must fail cleanly
    // (it would otherwise leak OUR memory at the allocator's vaddr to the
    // file — here it would read `own`'s contents, not the parent's buffer).
    let write_result = sys::buffer::write_to_file(file, transferred, 16);
    if write_result >= 0 {
        environment::log("FAIL: write_to_file from non-owned buffer succeeded");
        return 1;
    }
    environment::log("Buffer owner child: write from non-owned buffer refused");

    // Test 3: OP_BUFFER_RESIZE on a non-owned buffer must fail cleanly. If
    // it instead dereferences (or reallocates/replaces) the allocator's
    // vaddr in OUR address space, it would touch `own`'s memory directly, or
    // (on the reallocation path) call `free_buffer_vaddr` on OUR allocator
    // with the parent's vaddr — poisoning it for the next allocation below,
    // just like the free-safety check above.
    let resize_result = sys::buffer::resize(transferred, BUFFER_SIZE * 2, None);
    if resize_result >= 0 {
        environment::log("FAIL: resize of non-owned buffer succeeded");
        return 1;
    }
    if own.as_slice().iter().any(|&b| b != CHILD_MARKER) {
        environment::log("FAIL: resize of non-owned buffer corrupted our own buffer");
        return 1;
    }
    environment::log("Buffer owner child: resize of non-owned buffer refused");

    // Test 4: OP_BUFFER_FREE on a transferred handle must drop only the
    // handle — never reclaim the allocator's vaddr range into OUR vaddr
    // allocator. If it does, the next allocation below is placed on top of
    // `own`, and writing it corrupts `own`.
    if sys::buffer::free(transferred) < 0 {
        environment::log("FAIL: freeing a transferred handle should still close it");
        return 1;
    }
    let Some(mut next) = Buffer::alloc(BUFFER_SIZE) else {
        environment::log("FAIL: post-free alloc failed");
        return 1;
    };
    next.as_mut_slice().fill(0xEE);
    if own.as_slice().iter().any(|&b| b != CHILD_MARKER) {
        environment::log("FAIL: free of transferred handle poisoned our vaddr allocator");
        return 1;
    }
    environment::log("Buffer owner child: free of transferred handle is safe");

    0
}
