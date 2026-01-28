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
- Message-passing IPC: channels (bidirectional, 4KB messages, 16 depth) and mailboxes (event aggregation)
- Userspace: libpanda (two-layer architecture: sys:: low-level + high-level RAII wrappers), init, terminal, hello/ls/cat utilities, 28 test suites
- Unified device paths with class-based addressing (`keyboard:/pci/input/0`, `block:/pci/storage/0`)
- Cross-scheme device discovery via `*:` prefix (`*:/pci/storage/0` lists supporting schemes)
- Stdio handle infrastructure: STDIN=0, STDOUT=1, STDERR=2, with spawn supporting stdin/stdout redirection
- Structured Value type for pipeline data (Null, Bool, Int, Float, String, Bytes, Array, Map, Styled, Link, Table)
- Terminal IPC protocol with Request/Event messages and Value rendering
  
## Bugs / technical debt

- **Better userspace heap allocator**: The current allocator in `libpanda/src/heap.rs` is a simple bump allocator that grows the heap via `brk()` but never reuses freed memory. Replace with a proper allocator (e.g., linked-list, buddy, or dlmalloc-style) that tracks free blocks and reuses them.

- **Document magic numbers**: Several hardcoded values lack explanation:
  - `PRINT_BUFFER_SIZE: usize = 256` in print.rs
  - Keyboard scan codes in keyboard.rs should reference Linux input event codes


## Next steps

### 1. Missing syscalls

- **Implement OP_PROCESS_SIGNAL**: Basic signal support (at minimum SIGKILL/SIGTERM). Needed for killing processes from terminal.

- **Implement OP_ENVIRONMENT_TIME**: Return current time. Could use ACPI PM timer, TSC, or RTC. Needed for timing-sensitive applications.

### 2. System services

- **Implement system initialisation tool**: Declarative service configurations, similar to `systemd` on Linux, to describe services to start at boot.

### 3. Block I/O optimisations

- **Scatter-gather support**: Submit multiple non-contiguous sectors in one virtio request for better throughput.

- **Read-ahead**: Prefetch subsequent sectors while returning current data to reduce latency for sequential reads.

- **Write coalescing**: Batch multiple small writes into single larger requests to reduce virtio overhead.

### 4. Future work

- **Ext2 write support**: Currently ext2 is read-only.

- **Multi-CPU support**: APIC infrastructure exists but no SMP/IPI support.

- **CI setup**: Add GitHub Actions to run `make test` on push/PR.

## Known issues

- **proc-macro2 >= 1.0.104 causes test failures**: The `log!` macros generate incorrect code when used in x86-interrupt handlers with proc-macro2 1.0.104+. Cargo.lock pins proc-macro2 to 1.0.103 as a workaround.

- **ConfigurationAccess::unsafe_clone unimplemented**: virtio_gpu/mod.rs has a `todo!()` in the PCI configuration access trait impl. Not called in normal operation.

- **ACPI handler incomplete**: 27 `todo!()` macros in acpi/handler.rs for memory read/write operations. Not needed for current boot path.

## Design documents

- [plans/DEVICE_PATHS.md](plans/DEVICE_PATHS.md) - Unified device path scheme with human-friendly names
- [plans/STRUCTURED_PIPELINES.md](plans/STRUCTURED_PIPELINES.md) - Structured Value-based pipelines (replacing TERMINAL_IPC.md)
