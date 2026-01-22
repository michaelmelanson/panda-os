# Panda OS TODO

## Current state

Working:
- UEFI boot, memory management, page tables, demand-paged stack/heap
- Preemptive multitasking with full context switching
- Syscall ABI with callee-saved register preservation
- VFS with tarfs (initrd), resource scheme system
- Virtio GPU with Surface API (blit, fill, flush), virtio keyboard with blocking reads
- Virtio block device driver with byte-level access (sector alignment handled internally)
- Process handles: spawn returns handle, OP_PROCESS_WAIT blocks until child exits
- Userspace: libpanda, init, terminal (with keyboard input and font rendering), 12 test suites

Not yet implemented:
- `OP_PROCESS_SIGNAL`, `OP_ENVIRONMENT_TIME`
- ACPI handler read/write methods (27 todo!() macros)

## Next steps

1. **Implement OP_ENVIRONMENT_TIME**: Return current time. Could use ACPI PM timer, TSC, or RTC. Needed for timing-sensitive applications.

2. **Make terminal execute commands**: Currently terminal just echoes input. Parse command line, spawn programs from initrd (e.g., `spawn file:/initrd/program`).

3. **Add simple filesystem (FAT or ext2-readonly)**: Mount a disk image. Start with read-only access.

4. **Implement OP_PROCESS_SIGNAL**: Basic signal support (at minimum SIGKILL/SIGTERM). Needed for killing processes.

5. **Implement block device discovery**: Add `readdir` support to `BlockScheme` to list available block devices via `block:/` path. Currently devices must be accessed by known PCI address (e.g., `block:/pci/00:04.0`).

## Technical debt

No known technical debt

## Known issues

- **proc-macro2 >= 1.0.104 causes test failures**: The `log!` macros generate incorrect code when used in x86-interrupt handlers with proc-macro2 1.0.104+. Cargo.lock pins proc-macro2 to 1.0.103 as a workaround.

- **ConfigurationAccess::unsafe_clone unimplemented**: virtio_gpu/mod.rs has a `todo!()` in the PCI configuration access trait impl.

- **ACPI handler incomplete**: 27 `todo!()` macros in acpi/handler.rs for memory read/write operations.
