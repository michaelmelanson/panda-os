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
- Userspace: libpanda (two-layer architecture: sys:: low-level + high-level RAII wrappers), init, terminal, hello/ls/cat utilities, 25 test suites
- Unified device paths with class-based addressing (`keyboard:/pci/input/0`, `block:/pci/storage/0`)
- Cross-scheme device discovery via `*:` prefix (`*:/pci/storage/0` lists supporting schemes)
- Stdio handle infrastructure: STDIN=0, STDOUT=1, STDERR=2, with spawn supporting stdin/stdout redirection
  
## Bugs / technical debt

- **Better userspace heap allocator**: The current allocator in `libpanda/src/heap.rs` is a simple bump allocator that grows the heap via `brk()` but never reuses freed memory. Replace with a proper allocator (e.g., linked-list, buddy, or dlmalloc-style) that tracks free blocks and reuses them.


## Next steps

### 1. Structured pipelines (see plans/STRUCTURED_PIPELINES.md)

Enable shell pipelines (`cmd1 | cmd2 | cmd3`) where tools exchange structured `Value` objects rather than raw bytes. PowerShell-style object pipeline with Unix compatibility.

**Phase 1: Create Value type and restructure protocol**
- [ ] Create `panda-abi/src/value.rs` with `Value` enum (Null, Bool, Int, Float, String, Bytes, Array, Map, Styled, Link, Table)
- [ ] Create `Table` struct with `cols: u16`, `headers: Option<Vec<Value>>`, `cells: Vec<Value>`
- [ ] Implement `Encode`/`Decode` traits for `Value` and `Table`
- [ ] Add `Value::to_bytes()` / `Value::from_bytes()` helpers
- [ ] Rename `TerminalOutput` -> `Request`, `TerminalInput` -> `Event`
- [ ] Remove `Request::Write(Output)` - data goes through STDOUT
- [ ] Add `Request::Error(Value)` and `Request::Warning(Value)` for side-band errors
- [ ] Remove `Output`, `StyledText`, `StyledSpan` (subsumed by `Value`)

**Phase 2: Add channel create syscall**
- [ ] Add `OP_CHANNEL_CREATE` to `panda-abi/src/lib.rs`
- [ ] Implement `handle_create()` in `panda-kernel/src/syscall/channel.rs`
- [ ] Wire up in syscall dispatcher

**Phase 3: Update libpanda**
- [ ] Update `terminal.rs` to use `Request`/`Event` via PARENT only
- [ ] Update `stdio.rs`: `write_value(Value)` with STDOUT->PARENT fallback
- [ ] Update `print.rs`: `print!`/`println!` send `Value::String`
- [ ] Add `Channel::create_pair()` to `ipc/channel.rs`

**Phase 4: Update terminal emulator**
- [ ] Parse `|` in command lines
- [ ] Create data channels between pipeline stages
- [ ] Spawn processes with STDIN/STDOUT redirection
- [ ] Handle `Request` from any child, render `Value` from final stage
- [ ] Implement rendering for all `Value` variants

**Phase 5: Update tools**
- [ ] Update `cat` to output `Value::String` (or `Value::Map` for JSON)
- [ ] Update `ls` to output `Value::Table` with styled cells

**Phase 6: Add tests**
- [ ] `value_test/` - Value serialization, Table validation
- [ ] `pipeline_test/` - Multi-stage pipeline with Value flow
- [ ] `control_plane_test/` - Request/Event via PARENT

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
- [plans/STRUCTURED_PIPELINES.md](plans/STRUCTURED_PIPELINES.md) - Structured Value-based pipelines (replacing TERMINAL_IPC.md)
