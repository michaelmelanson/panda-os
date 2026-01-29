# Kernel internals

This document describes the internal architecture of the Panda kernel.

## Syscall entry and exit

The syscall handler is implemented in `panda-kernel/src/syscall/mod.rs` with the assembly entry point in `panda-kernel/src/syscall/entry.rs`.

### Entry

All syscalls use the x86-64 `syscall` instruction. The entry point performs a `swapgs` to access per-CPU kernel data, saves the user stack pointer, switches to the kernel stack, and pushes callee-saved registers. It then calls the Rust handler with references to the saved state.

Currently all syscalls go through a unified `SYSCALL_SEND` interface. The first argument is a handle, the second is an operation code, and the remaining four are operation-specific. The handler dispatches to operation-specific functions based on the operation code.

### Normal return

For syscalls that complete immediately, the return value is placed in `rax`, callee-saved registers are restored, and `sysretq` returns to userspace. This is the fast path.

### Blocking return

When a syscall cannot complete (e.g., reading from an empty keyboard buffer), it calls `ctx.block_on(waker)`. This saves the full register state, adjusts RIP back by 2 bytes to point at the `syscall` instruction itself, marks the process as blocked, and switches to another process. When the waker fires, the process becomes runnable again. On resume, all registers are restored and the `syscall` instruction re-executes from the beginning.

This re-execution model is simple but requires syscalls to be idempotent. See TODO.md for notes on potentially switching to an io_uring-style model.

## Process management

Processes are defined in `panda-kernel/src/process/mod.rs`. Each process has a unique `ProcessId`, a state (Runnable, Running, or Blocked), and timing information for fair scheduling.

The process holds its page table via a `Context` struct, its instruction and stack pointers, and a vector of memory mappings for code and data segments loaded from the ELF. The stack and heap are separate demand-paged mappings that grow as needed.

Each process has a `HandleTable` mapping handle IDs to kernel resources, and optionally a `SavedState` containing all CPU registers when the process isn't running. There's also an `Arc<ProcessInfo>` that survives process exit and is shared with any handles to the process (so waiters can retrieve the exit code).

For async syscalls like file I/O or spawn, the process stores a `PendingSyscall` containing a boxed future. The scheduler polls this future when the process is scheduled; when it completes, the result is returned to userspace.

All fields are private and accessed through methods.

### Process states

Processes transition between three states. A **Runnable** process is ready to execute and sits in the scheduler queue. The **Running** process is the one currently executing on the CPU (exactly one at a time in this single-core kernel). A **Blocked** process is waiting for something—a waker to fire, or an async future to complete.

When a process yields or blocks, it moves from Running to Runnable or Blocked. When a waker fires or a future becomes ready, the process moves from Blocked to Runnable. The scheduler picks from Runnable processes based on which was least recently scheduled.

### Saved state

The `SavedState` struct preserves all 16 general-purpose registers plus the instruction pointer and flags. It's populated when a process blocks or is preempted, and restored when the process resumes. For blocking syscalls, the saved RIP points at the `syscall` instruction so it re-executes on resume.

## Scheduler

The scheduler lives in `panda-kernel/src/scheduler/mod.rs`. It manages both userspace processes and kernel async tasks, treating them uniformly as `SchedulableEntity` values.

### Design

The scheduler uses min-heaps keyed by last-scheduled time (reversed, so the oldest is picked first). There's a separate heap for each process state. This provides fair round-robin scheduling where no process starves.

A timer interrupt fires every 10ms to preempt long-running processes. The interrupt handler saves the current process's state and returns to the scheduler loop.

### Main loop

The `exec_next_runnable()` function runs forever, picking the next entity to run. For a process with a pending async syscall, it polls the future first—if ready, it returns the result to userspace; if pending, the process goes back to Blocked and the loop continues.

For a normal process, it jumps to userspace. If there's saved state (from preemption or blocking), it uses `return_from_interrupt` to restore all registers. Otherwise it uses the faster `return_from_syscall` path.

For kernel tasks, it polls the future once. Completed tasks are removed; pending ones go to Blocked.

### Key operations

`add_process()` puts a new process in the Runnable queue. `block_current_on()` saves state, registers a waker, marks the process Blocked, and switches away (it doesn't return). `yield_current()` is similar but keeps the process Runnable. `wake_process()` moves a Blocked process to Runnable. `with_current_process()` runs a closure with mutable access to the current process.

## Waker system

Wakers enable async I/O by letting devices wake blocked processes. They're defined in `panda-kernel/src/process/waker.rs`.

### Device wakers

The `Waker` struct has a signaled flag and an optional waiting process ID. Devices create wakers and return them when an operation would block. The syscall layer then blocks the process on that waker. When the device has data (typically in an interrupt handler), it calls `wake()`, which sets the signaled flag and moves the waiting process to Runnable.

The `is_signaled()` method allows checking without blocking, and `clear()` resets the flag after consuming data.

### Future wakers

For Rust async/await integration, `ProcessWaker` implements the standard `Wake` trait. When a future's waker is invoked, it calls `wake_process()` to make the associated process runnable. This connects the kernel's async executor to the process scheduler.

## Context switching

The kernel has two ways to return to userspace, both in `panda-kernel/src/process/exec.rs`.

`return_from_syscall` is the fast path: it sets `rax` to the return value and uses `sysretq` to jump to the saved instruction pointer. This is used for completed syscalls and fresh process starts.

`return_from_interrupt` is the full restore path: it loads all 16 GPRs plus RIP, RSP, and RFLAGS from a `SavedState`, then uses `iretq`. This is used when resuming after preemption or blocking, where we need to restore arbitrary saved state rather than just returning from the current syscall.

### Preemption

When the timer interrupt fires, the CPU pushes an interrupt frame. The handler saves the remaining registers into `SavedState`, moves the process to Runnable, and jumps back to the scheduler loop. The next time this process is scheduled, `return_from_interrupt` restores its state exactly.

## Handle table

Each process has a handle table in `panda-kernel/src/handle.rs` mapping 32-bit handle IDs to kernel resources.

Handle IDs encode a type tag in the upper 8 bits and an ID in the lower 24 bits. This lets userspace verify handle types at runtime without a syscall. Handle IDs 0-6 are reserved for well-known handles (stdin, stdout, stderr, process, environment, mailbox, parent).

Each handle wraps an `Arc<dyn Resource>` plus a per-handle read offset for file-like resources. The table uses a `BTreeMap` internally and tracks the next available ID.

## Resource trait

Resources are kernel objects accessible via handles, defined in `panda-kernel/src/resource/mod.rs`. The `Resource` trait uses dynamic dispatch through `as_*` methods—each resource implements whichever interfaces it supports.

For example, a keyboard resource implements `as_event_source()` and `as_keyboard()`. A channel implements `as_channel()`. A file implements `as_vfs_file()`. Callers check which interfaces are available and use the appropriate one.

Resources can also provide a waker for blocking, report supported events for mailbox integration, and attach to mailboxes for event notification.

## Memory management

Each process has a `Context` holding its page table physical address. Memory mappings track virtual address ranges and their backing: pre-allocated frames for code/data loaded from the ELF, MMIO for device memory, or demand-paged for stack and heap.

Demand paging means the mapping exists but physical frames aren't allocated until accessed. When the process touches an unmapped address, a page fault occurs. If the address falls within a demand-paged region, the kernel allocates a frame, maps it, and resumes execution. If the address is invalid, the process is terminated.

The heap grows via the `brk` syscall, which adjusts the heap mapping size. The stack grows downward automatically through demand paging.

See [VIRTUAL_ADDRESS_SPACE.md](VIRTUAL_ADDRESS_SPACE.md) for the memory layout.

## Kernel task executor

The kernel includes an async executor for internal tasks, used by syscalls that need to do async work (file I/O, process spawning). Tasks are spawned with a future and get a `TaskId`.

Kernel tasks are scheduled alongside userspace processes as `SchedulableEntity::KernelTask`. They're polled once per schedule—if the future completes, the task is removed; if pending, it goes to Blocked until its waker fires.

## Key source files

| File | Description |
|------|-------------|
| `syscall/mod.rs` | Syscall handler dispatch |
| `syscall/entry.rs` | Assembly entry/exit |
| `scheduler/mod.rs` | Process and task scheduling |
| `process/mod.rs` | Process struct and lifecycle |
| `process/state.rs` | SavedState for context switches |
| `process/waker.rs` | Waker abstractions |
| `process/exec.rs` | return_from_syscall/interrupt |
| `handle.rs` | Handle table |
| `resource/mod.rs` | Resource trait and interfaces |
| `memory/mapping.rs` | Memory mappings |
