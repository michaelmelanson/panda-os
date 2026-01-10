# Kernel internals

## Syscall entry and exit

The syscall handler is in `panda-kernel/src/syscall.rs`.

### Entry (`syscall_entry`)

1. `swapgs` to switch to kernel GS base
2. Save user RSP to `gs:[0]`
3. Switch to kernel stack
4. Push callee-saved registers (rbx, rbp, r12-r15)
5. Push return RIP and RFLAGS
6. Call `syscall_handler` with arguments

### Exit (normal return)

1. Pop return RIP and RFLAGS
2. Pop callee-saved registers
3. Restore user RSP
4. `swapgs` and `sysretq`

### Exit (blocking syscall)

When a syscall blocks (returns `WouldBlock`), the normal return path is bypassed:

1. Full register state saved in `SavedState` struct
2. Process marked as `Blocked`
3. Scheduler switches to another process
4. On resume, `exec_userspace_with_state` restores all registers and re-executes syscall

## Process states

```
Runnable  <---> Running
    ^             |
    |             v
    +-------- Blocked
```

- `Runnable`: Ready to run, in scheduler queue
- `Running`: Currently executing (exactly one process)
- `Blocked`: Waiting on a waker (keyboard input, etc.)

## Waker system

Devices use wakers for async notification:

```rust
// Device creates waker
let waker = Waker::new();

// On blocking read, process registers with waker
waker.set_waiting(pid);
scheduler::block_current_on(waker, ...);

// When device has data (e.g., in IRQ handler)
waker.wake();  // Moves process to Runnable
```

## Key files

- `syscall.rs` - Syscall entry/exit, handler dispatch
- `scheduler.rs` - Process scheduling, blocking, waking
- `process.rs` - Process struct, SavedState, exec_userspace
- `waker.rs` - Waker abstraction for blocking I/O
