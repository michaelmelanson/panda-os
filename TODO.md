# Panda OS TODO

## Current state

Working:
- UEFI boot, memory management, page tables, demand-paged stack/heap
- Preemptive multitasking with full context switching
- Syscall ABI with callee-saved register preservation
- VFS with tarfs (initrd), resource scheme system
- Virtio GPU with Surface API (blit, fill, flush), virtio keyboard with blocking reads
- Process handles: spawn returns handle, OP_PROCESS_WAIT blocks until child exits
- Userspace: libpanda, init, terminal (with keyboard input and font rendering), 12 test suites

Not yet implemented:
- `OP_PROCESS_SIGNAL`, `OP_ENVIRONMENT_TIME`
- ACPI handler read/write methods (27 todo!() macros)

## Next steps

1. **Implement OP_ENVIRONMENT_TIME**: Return current time. Could use ACPI PM timer, TSC, or RTC. Needed for timing-sensitive applications.

2. **Make terminal execute commands**: Currently terminal just echoes input. Parse command line, spawn programs from initrd (e.g., `spawn file:/initrd/program`).

3. **Implement virtio-blk driver**: Block device support for persistent storage. Reuse virtio HAL from keyboard/GPU.

4. **Add simple filesystem (FAT or ext2-readonly)**: Mount a disk image. Start with read-only access.

5. **Implement OP_PROCESS_SIGNAL**: Basic signal support (at minimum SIGKILL/SIGTERM). Needed for killing processes.

## Technical debt

- **Deadline scheduling for timers**: Compositor refresh currently uses simple interval checking in timer interrupt. Should implement deadline scheduling so timers fire precisely on time (needed for reliable 60fps composition and future real-time features).

- **Kernel tasks**: Add a concept of a 'kernel task' that's scheduled similarly to userspace and can be preempted, except it doesn't need to do a context switch. Turn the compositor into a kernel task.

## Known issues

- **proc-macro2 >= 1.0.104 causes test failures**: The `log!` macros generate incorrect code when used in x86-interrupt handlers with proc-macro2 1.0.104+. Cargo.lock pins proc-macro2 to 1.0.103 as a workaround.

- **ConfigurationAccess::unsafe_clone unimplemented**: virtio_gpu/mod.rs has a `todo!()` in the PCI configuration access trait impl.

- **ACPI handler incomplete**: 27 `todo!()` macros in acpi/handler.rs for memory read/write operations.
