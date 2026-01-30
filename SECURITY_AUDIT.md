# Security Audit Findings

This file contains actionable security recommendations from a comprehensive audit of the panda-os kernel.
Each section below corresponds to a recommended GitHub issue.

---

## Issue 1: Validate ELF segment addresses to prevent kernel memory corruption

**Labels:** security, critical

### Summary

A crafted ELF binary can map attacker-controlled data over kernel code or data structures because `p_vaddr` from ELF program headers is used directly as the mapping address with no validation.

### Details

**File:** `panda-kernel/src/process/elf.rs:46`

`header.p_vaddr` is used directly as the virtual address for mapping ELF segments. There is no check that the address falls within the userspace range (`< 0x0000_8000_0000_0000`). A crafted ELF binary could specify a segment address in kernel space, allowing attacker-controlled data to overwrite kernel code or data.

Additionally, `p_offset` and `p_filesz` (at `elf.rs:115-119`) are not validated against the actual ELF file size, enabling out-of-bounds reads of kernel memory during loading.

### Recommendation

- Reject any ELF segment where `p_vaddr` or `p_vaddr + p_memsz` falls outside `[0, USER_ADDR_MAX)`
- Validate that `p_offset + p_filesz <= elf_file_size`

### Severity

**Critical** -- allows arbitrary kernel memory corruption from userspace.

---

## Issue 2: Use checked arithmetic for surface/window size calculations

**Labels:** security, critical

### Summary

Integer overflow in surface blit operations allows kernel heap memory corruption.

### Details

**File:** `panda-kernel/src/syscall/surface.rs:111, 183, 392`

`params.width * params.height * 4` is computed in `u32` arithmetic. Values like `width=65536, height=65536` overflow to 0, bypassing the size check. The subsequent pixel copy loop iterates billions of times with wrapping offsets, causing out-of-bounds read/write of kernel heap memory.

The same overflow exists in window resize, where a tiny buffer is allocated but large dimensions are stored, enabling heap buffer overflow on subsequent blits.

### Recommendation

- Use `checked_mul` or `saturating_mul` for all surface/window size calculations
- Alternatively, use `usize` instead of `u32` for intermediate calculations and validate against a maximum surface size (e.g., 8192x8192)

### Severity

**Critical** -- allows kernel heap corruption from userspace via crafted syscall arguments.

---

## Issue 3: Cap kernel heap allocations from syscall-provided sizes

**Labels:** security, critical

### Summary

Multiple syscall handlers allocate kernel memory based on userspace-provided sizes with no upper bound, allowing a single syscall to exhaust the kernel heap.

### Details

**Files:**
- `panda-kernel/src/syscall/file.rs:86` -- `vec![0u8; buf_len]` where `buf_len` comes from syscall argument
- `panda-kernel/src/syscall/channel.rs:164` -- channel message allocation
- `panda-kernel/src/resource/buffer.rs:22` -- shared buffer allocation

A malicious process can pass an extremely large size value (e.g., `0xFFFFFFFF`) to any of these syscalls, causing the kernel to attempt a multi-gigabyte allocation. This either panics the kernel (OOM) or exhausts all available memory, denying service to all other processes.

### Recommendation

- Enforce maximum sizes for each syscall: e.g., max 1MB per read buffer, max 4KB per channel message, max 16MB per shared buffer
- Return an error code (e.g., `EINVAL` or `ENOMEM`) when the requested size exceeds the limit
- Consider a per-process memory quota system

### Severity

**Critical** -- any process can crash or DoS the entire system with a single syscall.

---

## Issue 4: Program FMASK MSR to sanitize flags on syscall entry

**Labels:** security, medium

### Summary

The FMASK MSR is not programmed during syscall initialization, allowing userspace to set dangerous CPU flags (TF, AC, DF, NT) that persist into kernel mode.

### Details

**File:** `panda-kernel/src/syscall/entry.rs:16-36`

The `SYSCALL` instruction uses the FMASK MSR to clear specific RFLAGS bits on entry. Since FMASK is not programmed (defaults to 0), no flags are cleared. This allows userspace to:

- **TF (Trap Flag):** Cause a debug exception on every kernel instruction, potentially leaking kernel execution flow
- **DF (Direction Flag):** Reverse the direction of string operations in kernel code, corrupting memory copies
- **AC (Alignment Check):** Cause alignment check exceptions on unaligned kernel memory accesses
- **NT (Nested Task):** Potentially interfere with IRET behavior

### Recommendation

Program the FMASK MSR in `entry::init()` to clear at minimum: IF, TF, DF, AC, and NT on syscall entry:

```rust
const FMASK_VALUE: u64 = 0x4_7700; // Clear IF, TF, DF, NT, AC, IOPL
wrmsr(IA32_FMASK, FMASK_VALUE);
```

### Severity

**Medium** -- allows userspace to influence kernel execution behavior.

---

## Issue 5: Distinguish page fault types in demand paging handler

**Labels:** security, critical

### Summary

The demand paging handler does not distinguish between not-present faults and protection-violation faults, silently granting write access to read-only pages.

### Details

**Files:**
- `panda-kernel/src/interrupts.rs:209`
- `panda-kernel/src/memory/demand_paging.rs:129-202`

The page fault handler checks `USER_MODE` but not `CAUSED_BY_WRITE` or `PROTECTION_VIOLATION`. When a write fault occurs on a read-only page in the heap range, the handler allocates a new writable page instead of killing the process. This effectively bypasses read-only permissions for any heap page.

This means:
- Code pages mapped read-only can be silently replaced with writable pages
- Any page protection set up by the kernel for heap memory is ineffective

### Recommendation

- Check `error_code & PROTECTION_VIOLATION` -- if set, the page was present but the access was disallowed; this should NOT trigger demand paging
- Only demand-page on not-present faults (bit 0 of error code is 0)
- On protection violations, kill the faulting process with a segmentation fault

### Severity

**Critical** -- bypasses kernel-enforced memory protection for userspace processes.

---

## Issue 6: Disable interrupts during without_write_protection

**Labels:** security, critical

### Summary

The `without_write_protection` helper clears CR0.WP without disabling interrupts, risking permanent write protection bypass if an interrupt fires or the closure panics.

### Details

**File:** `panda-kernel/src/memory/paging.rs:91-101`

The function temporarily clears the Write Protect bit in CR0 to allow writing to read-only pages. If:
1. An interrupt fires while WP is cleared, the interrupt handler runs with write protection disabled
2. The closure panics, write protection is never re-enabled
3. On an SMP system, another CPU could observe and exploit the window

Since there is no interrupt-disabling guard or panic-safe scope guard, any of these scenarios leaves the kernel running without write protection.

### Recommendation

- Disable interrupts (`cli`) before clearing WP and re-enable (`sti`) after restoring it
- Use a scope guard pattern to ensure WP is re-enabled even if the closure panics:

```rust
fn without_write_protection<F, R>(f: F) -> R where F: FnOnce() -> R {
    let _guard = InterruptDisableGuard::new();
    unsafe { clear_wp(); }
    let result = f();
    unsafe { set_wp(); }
    result
}
```

### Severity

**Critical** -- can lead to permanent kernel write protection bypass.

---

## Issue 7: Validate ext2 superblock fields from untrusted disk images

**Labels:** security, critical

### Summary

The ext2 filesystem driver trusts all fields from on-disk superblock and directory entries without validation, allowing a crafted filesystem image to crash the kernel.

### Details

**Files:**
- `panda-kernel/src/vfs/ext2/structs.rs:183` -- `1024 << log_block_size` overflows if `log_block_size >= 22`
- `panda-kernel/src/vfs/ext2/mod.rs:165` -- division by zero if `inodes_per_group = 0`
- `panda-kernel/src/vfs/ext2/structs.rs:196` -- division by zero if `blocks_per_group = 0`
- `panda-kernel/src/vfs/ext2/mod.rs:110,147,178` -- `unsafe { ptr::read(...) }` casts raw disk buffers to structs without size validation
- `panda-kernel/src/vfs/ext2/mod.rs:214-215,251-252` -- directory entry parsing reads past buffer end when `rec_len` is maliciously small
- `panda-kernel/src/vfs/ext2/mod.rs:222,259` -- `name_len` not validated against `rec_len`, causing out-of-bounds slice

A crafted ext2 image can trigger:
- Kernel panic via integer overflow (debug) or silent wraparound (release)
- Kernel panic via division by zero
- Out-of-bounds memory reads via directory entry parsing
- Undefined behavior via `ptr::read` past buffer boundaries

### Recommendation

1. Validate `log_block_size` is in range `[0, 6]` (1KB to 64KB blocks)
2. Validate `blocks_per_group > 0` and `inodes_per_group > 0`
3. Check that `pos + size_of::<DirEntryRaw>() <= block_buf.len()` before every `ptr::read`
4. Validate `name_len <= rec_len - 8` and `rec_len >= 12` (minimum valid entry)
5. Consider using `zerocopy::FromBytes` instead of `ptr::read` for safe struct parsing

### Severity

**Critical** -- a malicious disk image can crash or corrupt the kernel.

---

## Issue 8: Canonicalize VFS paths to prevent mount-point escape

**Labels:** security, critical

### Summary

Path resolution in the VFS layer does not canonicalize paths, allowing `..` components to escape mount point boundaries.

### Details

**Files:**
- `panda-kernel/src/vfs/mod.rs:158-189` -- `resolve_path` uses `starts_with` without canonicalization
- `panda-kernel/src/vfs/ext2/mod.rs:182-197` -- `lookup` does not filter `..` components
- `panda-kernel/src/vfs/tarfs.rs:46-57` -- no `..` sanitization in path storage

A path like `/initrd/../disk/secret` matches the `/initrd` mount point (since it starts with `/initrd`), and passes `../disk/secret` as the relative path to the filesystem. The ext2 driver resolves `..` using the actual ext2 directory entry, which walks to the parent directory. This enables:

- Escaping a mount point's subtree
- Accessing files on a different filesystem than intended
- Path confusion between the VFS layer and filesystem drivers

### Recommendation

Implement path canonicalization in the VFS layer before mount-point matching:

```rust
fn canonicalize(path: &str) -> String {
    let mut components = Vec::new();
    for component in path.split('/').filter(|s| !s.is_empty()) {
        match component {
            "." => {},
            ".." => { components.pop(); },
            c => components.push(c),
        }
    }
    format!("/{}", components.join("/"))
}
```

Apply this to all paths in `resolve_path`, `open`, `stat`, and `readdir`.

### Severity

**Critical** -- allows filesystem traversal beyond intended mount boundaries.

---

## Issue 9: Add handle count and process count limits

**Labels:** security, critical

### Summary

There are no limits on the number of handles per process or total processes in the system, enabling resource exhaustion attacks.

### Details

**Handle exhaustion:**
- `panda-kernel/src/handle.rs:165-166` -- Handle ID counter is a `u32` using 24 bits. After ~16M allocations it wraps, and `BTreeMap::insert` silently overwrites existing handles, allowing a new handle to alias an existing resource.
- No per-process handle limit exists. A process can open millions of handles (files, channels, buffers) consuming kernel memory for each.

**Process exhaustion:**
- `panda-kernel/src/syscall/environment.rs:186-294` -- No process count limit. A malicious process can fork-bomb the system by repeatedly spawning child processes.
- `panda-kernel/src/process/mod.rs:52-56` -- PID counter uses `Relaxed` ordering with no duplicate detection.

### Recommendation

1. **Handle limits:** Enforce a per-process handle limit (e.g., 1024). Return an error when exceeded.
2. **Handle ID safety:** Either use a 64-bit counter, or detect and reject wraparound.
3. **Process limits:** Enforce a system-wide process limit (e.g., 256). Return an error from spawn when exceeded.
4. **PID ordering:** Use `SeqCst` or `AcqRel` for the PID counter to prevent duplicates on SMP.

### Severity

**Critical** -- handle wraparound causes resource aliasing; no limits enable DoS.

---

## Issue 10: Replace debug_assert with assert for safety-critical bounds checks

**Labels:** security, medium

### Summary

Safety-critical bounds checks use `debug_assert` which is stripped in release builds, leaving the kernel vulnerable to memory corruption.

### Details

**File:** `panda-kernel/src/memory/address.rs:96-106`

```rust
pub fn heap_phys_to_virt(phys: PhysAddr) -> VirtAddr {
    debug_assert!(
        phys.as_u64() >= HEAP_PHYS_START,
        "Physical address {:#x} is below heap start",
        phys.as_u64()
    );
    // ... converts to virtual address
}
```

This function converts a physical address to a virtual address for heap access. The `debug_assert` validates that the physical address is within the expected heap range. In release mode (which the project supports via `make build-release`), this check is removed entirely.

If an invalid physical address is passed:
- In debug mode: panic with a clear error message
- In release mode: silently compute an incorrect virtual address, leading to reads/writes to arbitrary kernel memory

### Recommendation

Replace `debug_assert` with `assert` for all safety-critical checks, or better yet, return a `Result`:

```rust
pub fn heap_phys_to_virt(phys: PhysAddr) -> Option<VirtAddr> {
    if phys.as_u64() < HEAP_PHYS_START {
        return None;
    }
    // ...
}
```

Audit the entire codebase for other `debug_assert` uses that guard safety invariants.

### Severity

**Medium** -- safety checks silently disappear in release builds, enabling memory corruption.
