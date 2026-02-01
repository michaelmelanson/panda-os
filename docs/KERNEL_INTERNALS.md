# Kernel internals

This document describes the internal architecture of the Panda kernel.

## Syscall entry and exit

The syscall handler is implemented in `panda-kernel/src/syscall/mod.rs` with the assembly entry point in `panda-kernel/src/syscall/entry.rs`.

### Entry

All syscalls use the x86-64 `syscall` instruction. The entry point performs a `swapgs` to access per-CPU kernel data, saves the user stack pointer, switches to the kernel stack, and pushes callee-saved registers. It then calls the Rust handler with references to the saved state.

Currently all syscalls go through a unified `SYSCALL_SEND` interface. The first argument is a handle, the second is an operation code, and the remaining four are operation-specific. The handler dispatches to operation-specific functions based on the operation code.

### Dispatch

The handler has three phases. First, it checks for diverging operations (yield and exit) that manipulate the scheduler directly and never return a value. Second, it creates a `UserAccess` token (proving the process page table is active) and calls `build_future()` to dispatch to the appropriate handler. All non-diverging handlers return a `SyscallFuture` — a `Pin<Box<dyn Future<Output = SyscallResult> + Send>>`. Third, it calls `poll_and_dispatch()` to poll the future once.

### Immediate return

If the future resolves immediately (`Poll::Ready`), the result code is placed in `rax`. If the result includes a `WriteBack` (data to copy to userspace), that copy happens now while the page table is still active. Callee-saved registers are restored and `sysretq` returns to userspace.

### Deferred return

If the future returns `Poll::Pending`, it is stored as a `PendingSyscall` on the process, and the process yields to the scheduler. When the process is next scheduled, the scheduler polls the future again. On completion, it switches to the process's page table, performs any writeback, and returns the result to userspace via `return_from_syscall`.

### Handler patterns

Handlers fall into three categories:

- **Synchronous** handlers (close, brk, get_pid) compute their result immediately and wrap it in `core::future::ready()`.
- **Blocking** handlers (channel send/recv, mailbox wait, process wait, keyboard read) use `poll_fn` to retry an operation on each poll. They register a device waker on each `Pending` return so the process is woken when data is available.
- **Async I/O** handlers (file read/write, open, spawn, mount) build a `Box::pin(async { ... })` future that does asynchronous work through the VFS layer.

### Userspace memory safety

Handlers never access userspace memory directly. The `UserAccess` type is a `!Send` token that proves the current process's page table is active. It cannot be captured in a `Send` future, so the compiler prevents userspace access from inside async blocks. Instead, handlers follow a copy-in/copy-out discipline:

- **Copy-in**: Handlers that read from userspace (channel send, file write, spawn) receive a `&UserAccess` and copy data into kernel buffers before building their future.
- **Copy-out**: Handlers that produce data for userspace (channel recv, file read) capture a `UserSlice` (an opaque address+length pair) in their future and return the data via `WriteBack`. The top-level dispatch copies it out after the future completes.

The `UserSlice` type has private fields, so handler code cannot extract raw addresses. All handler modules have `#![deny(unsafe_code)]`, ensuring they cannot use `unsafe` to bypass these protections. Only `mod.rs` (top-level dispatch) and `user_ptr.rs` (the `UserAccess` implementation) contain `unsafe`.

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

The `SavedState` struct preserves all 16 general-purpose registers plus the instruction pointer and flags. It's populated when a process is preempted by a timer interrupt and restored when the process resumes. Blocking syscalls no longer save register state — they store a future instead.

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

`add_process()` puts a new process in the Runnable queue. `yield_current()` saves the resume point, marks the process Runnable, and switches to another entity (it doesn't return). `wake_process()` moves a Blocked process to Runnable. `with_current_process()` runs a closure with mutable access to the current process.

## Waker system

Wakers enable async I/O by letting devices wake blocked processes. They're defined in `panda-kernel/src/process/waker.rs`.

### Device wakers

The `Waker` struct has a signaled flag and an optional waiting process ID. Blocking syscall futures call `waker.set_waiting(pid)` on each `Pending` return to register the current process. When the device has data (typically in an interrupt handler), it calls `wake()`, which sets the signaled flag and moves the waiting process to Runnable. The scheduler then polls the future again.

The `is_signaled()` method allows checking without blocking, and `clear()` resets the flag after consuming data.

### Future wakers

For Rust async/await integration, `ProcessWaker` implements the standard `Wake` trait. When a future's waker is invoked, it calls `wake_process()` to make the associated process runnable. Both device wakers and Rust's `core::task::Waker` converge on the same `wake_process()` function.

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

## ELF loading and process startup

Process creation uses the `panda-elf` crate (`crates/panda-elf`), a minimal `no_std` ELF64 parser that reads only the ELF header (64 bytes) and program header table (N × 56 bytes). Section headers, symbol tables, string tables, relocations, and dynamic linking info are not parsed. This avoids the overhead of a full ELF parse library, which is significant in debug mode.

The parser validates: ELF magic, 64-bit class, little-endian encoding, and program header table bounds. For each PT_LOAD segment, security validation checks that virtual addresses and file offsets are within bounds and don't extend into kernel space.

### Page table batching

When mapping ELF segments, several optimisations reduce page table overhead:

1. **Batch TLB flushes**: Instead of issuing an `INVLPG` for each page, a single `flush_all` is issued after all mappings are complete. For new process page tables, the entries can't be in the TLB cache anyway.
2. **Cached page table levels**: For consecutive pages within the same 2MB region, the L4/L3/L2 entries are identical. The mapper caches the L1 page table pointer and only re-walks the hierarchy when crossing a 2MB boundary. This reduces page table walks from 2N to ~N/512.
3. **Pre-allocated frames**: All physical frames are allocated before mapping begins, separating allocation from page table manipulation for better cache locality.
4. **Pre-allocated Vec capacity**: The frame list is allocated with exact capacity upfront, avoiding repeated reallocation.

### 2MB huge page support

For regions >= 2MB, `allocate_and_map` uses 2MB huge pages via L2 page table entries with the `HUGE_PAGE` flag. This reduces page table entries by 512× for the huge-page portion and improves TLB coverage.

The mapping is split into three parts:
- **Head**: 4KB pages from the start up to the first 2MB boundary
- **Body**: 2MB huge pages for each complete 2MB-aligned region
- **Tail**: 4KB pages for the remainder after the last 2MB boundary

The `Mapping::write_at` method handles variable-sized frames (4KB and 2MB) transparently. The unmap path already handled huge pages prior to this optimisation.

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
| `syscall/mod.rs` | Syscall dispatch, poll-once, copy-out |
| `syscall/user_ptr.rs` | UserAccess, UserSlice, SyscallResult |
| `syscall/entry.rs` | Assembly entry/exit |
| `scheduler/mod.rs` | Process and task scheduling |
| `process/mod.rs` | Process struct and lifecycle |
| `process/state.rs` | SavedState for context switches |
| `process/waker.rs` | Waker abstractions |
| `process/exec.rs` | return_from_syscall/interrupt |
| `handle.rs` | Handle table |
| `resource/mod.rs` | Resource trait and interfaces |
| `memory/mapping.rs` | Memory mappings |
| `memory/paging.rs` | Page table operations, huge page support |
| `process/elf.rs` | Minimal ELF parser and segment loading |
