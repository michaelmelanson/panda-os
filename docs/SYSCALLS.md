# Syscall architecture

The kernel uses a resource-oriented syscall design with a single entry point.

## Design

- Single `send(handle, operation, args...)` syscall
- Well-known handles: `HANDLE_SELF` (0), `HANDLE_ENVIRONMENT` (1)
- Operation codes grouped by resource type (File, Process, Environment)
- Type-safe handle table in kernel prevents wrong operations on handles

See `panda-abi/src/lib.rs` for operation constants.

## Syscall ABI

The syscall uses the standard x86_64 syscall convention:

- `rax`: syscall code (always `SYSCALL_SEND = 0x30`)
- `rdi`: handle
- `rsi`: operation code
- `rdx`, `r10`, `r8`, `r9`: operation arguments
- Return value in `rax`

The `syscall` instruction clobbers `rcx` (saves RIP) and `r11` (saves RFLAGS).

## Callee-saved register preservation

The kernel preserves callee-saved registers (rbx, rbp, r12-r15) across syscalls:

- **Normal syscalls**: Registers are saved on the kernel stack in `syscall_entry` and restored before `sysretq`
- **Blocking syscalls**: When a syscall blocks (e.g., reading from keyboard with no input), the kernel saves all registers in `SavedState` so they can be restored when the process resumes
- **Yield**: Process yields don't need register restoration since they return immediately with rax=0

## Blocking and wakers

When a syscall cannot complete immediately (e.g., `OP_FILE_READ` on a keyboard with no input):

1. The syscall handler returns `FsError::WouldBlock(waker)`
2. The kernel saves the process state including:
   - Return IP (pointing to syscall instruction for re-execution)
   - User stack pointer
   - All syscall argument registers
   - All callee-saved registers
3. The process is marked as `Blocked` and the waker is registered
4. When data becomes available, the device calls `waker.wake()`
5. The process resumes and re-executes the syscall

## Userspace API

Use the libpanda API rather than raw syscalls:

```rust
use libpanda::environment;  // System operations
use libpanda::file;         // File operations
use libpanda::process;      // Process operations

// Environment operations (via HANDLE_ENVIRONMENT)
environment::log("message");           // Log to console
environment::open("/path", flags);     // Open file, returns handle
environment::spawn("/path");           // Spawn process

// File operations (on file handles)
file::read(handle, &mut buf);          // Read from file
file::write(handle, &buf);             // Write to file
file::close(handle);                   // Close file

// Process operations (via HANDLE_SELF)
process::yield_now();                  // Yield CPU
process::exit(code);                   // Exit process
```
