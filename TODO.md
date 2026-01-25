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
- Message-passing IPC: channels (bidirectional, 1KB messages, 16 depth) and mailboxes (event aggregation)
- Userspace: libpanda, init, terminal (with command execution), hello/ls/cat utilities, 25 test suites
- Unified device paths with class-based addressing (`keyboard:/pci/input/0`, `block:/pci/storage/0`)
- Cross-scheme device discovery via `*:` prefix (`*:/pci/storage/0` lists supporting schemes)
  
## Bugs / technical debt

- **Add high-level wrappers around `panda-abi` primitives**: The API for operations in `panda-abi` should have well-designed, idiomatic Rust wrappers around the current low-level operations and system calls.

- **`Event` structure should be organized around resources and their events**: e.g. `Key(KeyEvent)` -> `Input(InputEvent::Key(KeyEvent))`

- **Framebuffer should recomposite dirty regions from scratch**: Currently it's painting over the existing content, rather than clearing it and repainting from scratch.

## Next steps

### 1. Terminal command execution (see plans/TERMINAL_COMMANDS.md)

#### Phase 1: Mailbox + Channel infrastructure
- [x] Add mailbox/channel syscalls and constants to panda-abi
- [x] Implement Mailbox resource in kernel (event aggregation, waker support)
- [x] Implement ChannelEndpoint resource in kernel (message queues, 1KB max, 16 depth)
- [x] Add Resource trait methods: `supported_events()`, `poll_events()`, `attach_mailbox()`
- [x] Implement mailbox syscall handlers (create, wait, poll)
- [x] Implement channel syscall handlers (send, recv with NONBLOCK flag)
- [x] Update open/spawn syscalls to take mailbox + event_mask parameters
- [x] Create default mailbox (HANDLE_MAILBOX) on process creation
- [x] Add libpanda mailbox module (Mailbox, Event enum, recv/try_recv)
- [x] Update libpanda channel module (send/try_send, recv/try_recv)
- [x] Update libpanda environment module (open/spawn with mailbox)

#### Phase 2: Spawn creates channel
- [x] Modify handle_spawn to create channel pair
- [x] Child gets HANDLE_PARENT channel attached to its default mailbox
- [x] Create SpawnHandle resource (channel + process info)

#### Phase 3: Startup message protocol
- [x] Add StartupMessageHeader to panda-abi
- [x] Add libpanda startup module (encode/decode args)

#### Phase 4: Userspace API
- [x] Update spawn() to take args slice, send startup message over channel
- [x] Update main! macro to receive startup message and parse args

#### Phase 5: Terminal rewrite
- [x] Rewrite terminal with mailbox event loop
- [x] Add command parsing and path resolution
- [x] Spawn child processes with args, wait for exit

#### Phase 6: Basic utilities
- [x] Create `hello` program
- [x] Create `ls` program (with args support)
- [x] Create `cat` program (with args support)
- [x] Update Makefile to build and include in ext2 image

### 2. Terminal IPC protocol (see plans/TERMINAL_IPC.md)

A structured message-passing protocol between terminal and child processes, replacing character-oriented VT100/ANSI. Key features:
- Typed messages over channels (Write, SetStyle, RequestInput, etc.)
- Generic `Output` enum: `Text`, `StyledText`, `Image`, `Table`, `Link`, etc.
- ANSI compatibility layer for legacy software
- Clean libpanda::terminal API for common operations

### 3. Missing syscalls

- **Implement OP_PROCESS_SIGNAL**: Basic signal support (at minimum SIGKILL/SIGTERM). Needed for killing processes from terminal.

- **Implement OP_ENVIRONMENT_TIME**: Return current time. Could use ACPI PM timer, TSC, or RTC. Needed for timing-sensitive applications.

### 4. System services

- **Implement system initialization tool**: Declarative service configurations, similar to `systemd` on Linux, to describe services to start at boot.

### 5. Block I/O optimizations

- **Scatter-gather support**: Submit multiple non-contiguous sectors in one virtio request for better throughput.

- **Read-ahead**: Prefetch subsequent sectors while returning current data to reduce latency for sequential reads.

- **Write coalescing**: Batch multiple small writes into single larger requests to reduce virtio overhead.

### 6. Type safety improvements

- **Convert panda-abi constants to enums**: The syscall opcodes, event flags, channel flags, and handle constants are all raw `u32`/`usize` values. Should use proper enums with `#[repr(u32)]` for type safety. This would catch misuse at compile time (e.g., passing an event flag where a syscall opcode is expected).

### 7. Future work

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
- [plans/TERMINAL_COMMANDS.md](plans/TERMINAL_COMMANDS.md) - Terminal command execution with mailbox/channel IPC
- [plans/TERMINAL_IPC.md](plans/TERMINAL_IPC.md) - Structured terminal IPC protocol (replacing VT100/ANSI)
