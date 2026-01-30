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

- **Implement system initialisation tool**: Declarative service manager with TOML configs, dependency resolution, restart policies, and runtime management. See [plans/system-init-tool.md](plans/system-init-tool.md) for the full design.

  1. **Signal support**: Implement `OP_PROCESS_SIGNAL` for SIGKILL (kernel-level forced termination). Add `Signal::Terminate` variant for message-based SIGTERM delivery via `HANDLE_PARENT` channel.
  2. **Timer resource**: Mailbox-integrated one-shot timer resource (`EVENT_TIMER`). Extends the existing `DeadlineTracker`/APIC timer infrastructure to post events to userspace mailboxes.
  3. **Service protocol framework**: `Protocol` and `Service` traits in `panda-abi` with UUID identification and capability negotiation. `ProtocolChannel<P>` and `ServiceClient<S>` typed wrappers in `libpanda`. Common handshake (Hello/Welcome/Rejected) and message framing (Kind byte + TLV).
  4. **Service scheme**: Kernel-side `service:` scheme that brokers channel connections to init. Init registers via `OP_SERVICE_REGISTER`; clients open `service:/manager` to get a channel. Handshake uses the protocol framework.
  5. **TOML parsing and config**: Add `toml` crate (v0.9, `no_std`) to init. `ServiceConfig` parser reads `/config/services/{name}/config.toml`.
  6. **Planner**: Stateless planner that diffs current vs desired state, detects cycles, topologically sorts, and produces a DAG of Start/Stop/Restart actions. Used at boot and for runtime changes.
  7. **Service manager core**: Event loop in init: executes plan steps, handles process exits, restart timers, stop grace periods, log forwarding, and command dispatch. All events flow through a single mailbox.
  8. **Service manager API crate and `svcctl`**: `service-manager-api` crate with typed `ManagerRequest`/`ManagerResponse`/`ManagerEvent`. `svcctl` CLI uses `ServiceClient<ServiceManager>` for runtime service management. Integrates with structured pipelines.
  9. **Service config files and boot test**: Add `rootfs/config/services/terminal/config.toml`. Verify system boots with the new service manager.

### 3. Block I/O optimisations

- **Scatter-gather support**: Submit multiple non-contiguous sectors in one virtio request for better throughput.

- **Read-ahead**: Prefetch subsequent sectors while returning current data to reduce latency for sequential reads.

- **Write coalescing**: Batch multiple small writes into single larger requests to reduce virtio overhead.

### 4. Security

- **Enable SMAP (Supervisor Mode Access Prevention)**: The kernel currently does not enable SMAP. With SMAP, the CPU faults when kernel code accesses user-mapped pages without explicit `stac`/`clac` bracketing. This prevents the kernel from accidentally dereferencing user pointers. Requires auditing all kernel code that intentionally accesses userspace memory (syscall handlers, `UserAccess`, shared buffer blit paths) and wrapping those accesses with `stac`/`clac`.

### 5. Future work

- **Ext2 write support**: Currently ext2 is read-only.

- **Multi-CPU support**: APIC infrastructure exists but no SMP/IPI support.

- **CI setup**: Add GitHub Actions to run `make test` on push/PR.

- **Declarative mount configuration**: Replace the hardcoded `mount("ext2", "/mnt")` in init with a declarative fstab-like config (e.g., `/config/fstab.toml`) specifying filesystem type, device, and mount point. Would allow init to mount multiple filesystems without recompilation.

- **GPU-accelerated composition**: Add virtio-gpu 3D (virgl) support to offload window composition to the host GPU. Currently the compositor does CPU-side pixel-by-pixel alpha blending. See [plans/virtio-gpu-3d-composition.md](plans/virtio-gpu-3d-composition.md) for the full design.

## Known issues

- **proc-macro2 >= 1.0.104 causes test failures**: The `log!` macros generate incorrect code when used in x86-interrupt handlers with proc-macro2 1.0.104+. Cargo.lock pins proc-macro2 to 1.0.103 as a workaround.

- **ConfigurationAccess::unsafe_clone unimplemented**: virtio_gpu/mod.rs has a `todo!()` in the PCI configuration access trait impl. Not called in normal operation.

- **ACPI handler incomplete**: 27 `todo!()` macros in acpi/handler.rs for memory read/write operations. Not needed for current boot path.

## Documentation

- [docs/DEVICE_PATHS.md](docs/DEVICE_PATHS.md) - Unified device path scheme with human-friendly names
- [docs/PIPELINES.md](docs/PIPELINES.md) - Structured Value-based pipelines
- [docs/IPC.md](docs/IPC.md) - Channels, mailboxes, and process communication
