# Panda OS TODO

## Current state

Working:
- UEFI boot, memory management, page tables, demand-paged stack/heap
- Higher-half kernel with recursive page tables and MMIO region (no physical memory window)
- Preemptive multitasking with full context switching
- Syscall ABI with callee-saved register preservation
- VFS with async traits (tarfs, ext2), resource scheme system
- Read-only ext2 filesystem driver with async I/O support
- Virtio GPU with Surface API (blit, fill, flush), virtio keyboard with blocking reads
- Virtio block device driver with async I/O (interrupt-driven, non-blocking)
- Process handles: spawn returns handle, OP_PROCESS_WAIT blocks until child exits
- Message-passing IPC: channels (bidirectional, 1KB messages, 16 depth) and mailboxes (event aggregation)
- Userspace: libpanda, init, terminal (with command execution), hello/ls/cat utilities, 25 test suites
- Unified device paths with class-based addressing (`keyboard:/pci/input/0`, `block:/pci/storage/0`)
- Cross-scheme device discovery via `*:` prefix (`*:/pci/storage/0` lists supporting schemes)
  
## Bugs / technical debt

- **Add high-level wrappers around `panda-abi` primitives**: The API for operations in `panda-abi` should have well-designed, idiomatic Rust wrappers around the current low-level operations and system calls.

- **Better userspace heap allocator**: The current allocator in `libpanda/src/heap.rs` is a simple bump allocator that grows the heap via `brk()` but never reuses freed memory. Replace with a proper allocator (e.g., linked-list, buddy, or dlmalloc-style) that tracks free blocks and reuses them.


## Next steps

### 1. Terminal IPC protocol (see plans/TERMINAL_IPC.md)

A structured message-passing protocol between terminal and child processes, replacing character-oriented VT100/ANSI. Key features:
- Typed messages over channels (Write, SetStyle, RequestInput, etc.)
- Generic `Output` enum: `Text`, `StyledText`, `Image`, `Table`, `Link`, etc.
- ANSI compatibility layer for legacy software
- Clean libpanda::terminal API for common operations

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

- **IPC/Pipes**: Implement pipe support for shell pipelines.

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
- [plans/TERMINAL_IPC.md](plans/TERMINAL_IPC.md) - Structured terminal IPC protocol (replacing VT100/ANSI)
