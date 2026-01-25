# Panda OS TODO

## Current state

Working:
- UEFI boot, memory management, page tables, demand-paged stack/heap
- Higher-half kernel with physical memory window and MMIO region
- Preemptive multitasking with full context switching
- Syscall ABI with callee-saved register preservation
- VFS with async traits (tarfs, ext2), resource scheme system
- Read-only ext2 filesystem driver with async I/O support
- Virtio GPU with Surface API (blit, fill, flush), virtio keyboard with blocking reads
- Virtio block device driver with async I/O (interrupt-driven, non-blocking)
- Process handles: spawn returns handle, OP_PROCESS_WAIT blocks until child exits
- Userspace: libpanda, init, terminal (with keyboard input and font rendering), 24 test suites
- Unified device paths with class-based addressing (`keyboard:/pci/input/0`, `block:/pci/storage/0`)
- Cross-scheme device discovery via `*:` prefix (`*:/pci/storage/0` lists supporting schemes)

Not yet implemented:
- `OP_PROCESS_SIGNAL`, `OP_ENVIRONMENT_TIME`
- ACPI handler read/write methods (27 todo!() macros)
- Message-passing IPC (channel.rs has stubs only)

## Next steps

### 1. Usability (make the system interactive)

- **Make terminal execute commands**: Currently terminal just echoes input. Parse command line, spawn programs (e.g., typing `hello` spawns `/mnt/hello`). Handle child process exit and return to prompt.

- **Basic file utilities**: Create simple programs for file operations:
  - `ls` - list directory contents
  - `cat` - print file contents
  - `echo` - print arguments

### 2. Missing syscalls

- **Implement OP_PROCESS_SIGNAL**: Basic signal support (at minimum SIGKILL/SIGTERM). Needed for killing processes from terminal.

- **Implement OP_ENVIRONMENT_TIME**: Return current time. Could use ACPI PM timer, TSC, or RTC. Needed for timing-sensitive applications.

### 3. System services

- **Implement system initialization tool**: Declarative service configurations, similar to `systemd` on Linux, to describe services to start at boot.

### 4. Block I/O optimizations

- **Scatter-gather support**: Submit multiple non-contiguous sectors in one virtio request for better throughput.

- **Read-ahead**: Prefetch subsequent sectors while returning current data to reduce latency for sequential reads.

- **Write coalescing**: Batch multiple small writes into single larger requests to reduce virtio overhead.

### 5. Future work

- **IPC/Pipes**: Implement pipe support for shell pipelines. The channel.rs module has stubs for message-passing but nothing is implemented yet.

- **Environment variables**: Support for PATH, HOME, etc. needed for proper shell operation.

- **Ext2 write support**: Currently ext2 is read-only.

- **Multi-CPU support**: APIC infrastructure exists but no SMP/IPI support.

- **CI setup**: Add GitHub Actions to run `make test` on push/PR.

## Known issues

- **proc-macro2 >= 1.0.104 causes test failures**: The `log!` macros generate incorrect code when used in x86-interrupt handlers with proc-macro2 1.0.104+. Cargo.lock pins proc-macro2 to 1.0.103 as a workaround.

- **ConfigurationAccess::unsafe_clone unimplemented**: virtio_gpu/mod.rs has a `todo!()` in the PCI configuration access trait impl. Not called in normal operation.

- **ACPI handler incomplete**: 27 `todo!()` macros in acpi/handler.rs for memory read/write operations. Not needed for current boot path.

## Design documents

- [plans/DEVICE_PATHS.md](plans/DEVICE_PATHS.md) - Unified device path scheme with human-friendly names
