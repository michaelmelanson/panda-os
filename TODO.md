# Panda OS TODO

## Current state

Working:
- UEFI boot, memory management, page tables, demand-paged stack/heap
- Preemptive multitasking with full context switching
- Syscall ABI with callee-saved register preservation
- VFS with tarfs (initrd), resource scheme system
- Virtio GPU (basic), virtio keyboard with blocking reads
- Process handles: spawn returns handle, OP_PROCESS_WAIT blocks until child exits
- Userspace: libpanda, init, shell (echo-only), 9 test suites

Not yet implemented:
- `OP_PROCESS_SIGNAL`, `OP_ENVIRONMENT_TIME`
- ACPI handler read/write methods (27 todo!() macros)

## Next steps

1. **Implement OP_ENVIRONMENT_TIME**: Return current time. Could use ACPI PM timer, TSC, or RTC. Needed for timing-sensitive applications.

2. **Make shell execute commands**: Currently shell just echoes input. Parse command line, spawn programs from initrd (e.g., `spawn file:/initrd/program`).

3. **Add directory listing to VFS**: Implement `OP_FILE_READDIR` or similar. Shell needs this for `ls` command.

4. **Implement virtio-blk driver**: Block device support for persistent storage. Reuse virtio HAL from keyboard/GPU.

5. **Add simple filesystem (FAT or ext2-readonly)**: Mount a disk image. Start with read-only access.

6. **Implement OP_PROCESS_SIGNAL**: Basic signal support (at minimum SIGKILL/SIGTERM). Needed for killing processes.

7. **GPU blitting/composition API**: The virtio-gpu driver just provides a framebuffer and flush. The kernel needs to manage this framebuffer and expose blitting/composition operations to userspace (e.g., create surface, blit surface to screen, flush region). A windowing system would allocate surfaces and the kernel composites them.

## Technical debt

None currently tracked.

## Known issues

- **proc-macro2 >= 1.0.104 causes test failures**: The `log!` macros generate incorrect code when used in x86-interrupt handlers with proc-macro2 1.0.104+. Cargo.lock pins proc-macro2 to 1.0.103 as a workaround.

- **ConfigurationAccess::unsafe_clone unimplemented**: virtio_gpu/mod.rs has a `todo!()` in the PCI configuration access trait impl.

- **ACPI handler incomplete**: 27 `todo!()` macros in acpi/handler.rs for memory read/write operations.
