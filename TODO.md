# Panda OS TODO

## Current state

Working:
- UEFI boot, memory management, page tables
- Preemptive multitasking with full context switching
- Syscall ABI with callee-saved register preservation
- VFS with tarfs (initrd), resource scheme system
- Virtio GPU (basic), virtio keyboard with blocking reads
- Userspace: libpanda, init, shell (echo-only), 9 test suites

Not yet implemented:
- `OP_PROCESS_WAIT`, `OP_PROCESS_SIGNAL`, `OP_ENVIRONMENT_TIME`
- Spawn returns 0 instead of process handle
- ACPI handler read/write methods (27 todo!() macros)

## Next steps

1. **Return process handle from spawn**: `OP_ENVIRONMENT_SPAWN` returns 0 instead of a handle. Return a proper handle so parent can wait on child.

2. **Implement OP_PROCESS_WAIT**: Allow parent to wait for child process to exit. Requires tracking parent-child relationships and storing exit codes.

3. **Implement OP_ENVIRONMENT_TIME**: Return current time. Could use ACPI PM timer, TSC, or RTC. Needed for timing-sensitive applications.

4. **Make shell execute commands**: Currently shell just echoes input. Parse command line, spawn programs from initrd (e.g., `spawn file:/initrd/program`).

5. **Add directory listing to VFS**: Implement `OP_FILE_READDIR` or similar. Shell needs this for `ls` command.

6. **Implement virtio-blk driver**: Block device support for persistent storage. Reuse virtio HAL from keyboard/GPU.

7. **Add simple filesystem (FAT or ext2-readonly)**: Mount a disk image. Start with read-only access.

8. **Implement OP_PROCESS_SIGNAL**: Basic signal support (at minimum SIGKILL/SIGTERM). Needed for killing processes.

9. **GPU blitting/composition API**: The virtio-gpu driver just provides a framebuffer and flush. The kernel needs to manage this framebuffer and expose blitting/composition operations to userspace (e.g., create surface, blit surface to screen, flush region). A windowing system would allocate surfaces and the kernel composites them.

## Technical debt

- **Reorganise kernel modules**: Some modules are getting large (e.g., syscall.rs, memory/mod.rs). Consider splitting into submodules as was done for scheduler/.

## Known issues

- **proc-macro2 >= 1.0.104 causes test failures**: The `log!` macros generate incorrect code when used in x86-interrupt handlers with proc-macro2 1.0.104+. Cargo.lock pins proc-macro2 to 1.0.103 as a workaround.

- **ConfigurationAccess::unsafe_clone unimplemented**: virtio_gpu/mod.rs has a `todo!()` in the PCI configuration access trait impl.

- **ACPI handler incomplete**: 27 `todo!()` macros in acpi/handler.rs for memory read/write operations.
