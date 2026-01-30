# Migrate syscall handlers to return futures

## Problem

Syscall handlers currently use two ad-hoc mechanisms to block:
- `ctx.block_on(waker)` — saves full register state, re-executes syscall from scratch on wake (5 call sites)
- `ctx.yield_for_async()` — stores a `PendingSyscall` future, yields to scheduler (11 call sites)

Both involve the handler managing its own suspend/resume. The handlers also need a `&SyscallContext` just to call these methods.

## Goal

Lift the yield/resume logic to the top level. All syscall handlers return a future. The top-level `syscall_handler` polls it once:
- `Poll::Ready(result)` — return via normal sysret
- `Poll::Pending` — store as `PendingSyscall`, yield to scheduler

This unifies both blocking models and removes `block_on`, `yield_for_async`, and `SyscallContext` from handler signatures.

## Design

### Top-level dispatch

Since yield and exit are special-cased first (they manipulate the scheduler directly and never return a value to userspace), everything else cleanly returns a future. No mixed diverging/returning types in the dispatch.

```rust
type SyscallFuture = Pin<Box<dyn Future<Output = SyscallResult> + Send>>;

fn syscall_handler(
    arg0: usize, ..., code: usize, return_rip: usize, user_rsp: usize,
) -> isize {
    let ua = unsafe { UserAccess::new() };

    // Phase 1: diverging ops that don't return a value
    if code == SYSCALL_SEND {
        let operation = arg1 as u32;
        match operation {
            OP_PROCESS_YIELD => process::handle_yield(return_rip, user_rsp),
            OP_PROCESS_EXIT => process::handle_exit(arg2 as i32),
            _ => {}
        }
    }

    // Phase 2: build future (SyscallError → return -1)
    let future: SyscallFuture = match build_future(&ua, operation, handle, ...) {
        Ok(f) => f,
        Err(_) => return -1,
    };

    // Phase 3: poll once, copy out on Ready
    poll_and_dispatch(future, return_rip, user_rsp)
}

/// Dispatch to the appropriate handler, which may use ua to copy
/// from userspace before building its future.
fn build_future(
    ua: &UserAccess,
    operation: u32,
    handle: u32,
    ...
) -> Result<SyscallFuture, SyscallError> {
    Ok(match operation {
        OP_CHANNEL_SEND => channel::handle_send(ua, handle, ...)?,
        OP_CHANNEL_RECV => channel::handle_recv(handle, ...)?,
        // Handlers that don't touch userspace can't fail with BadUserPointer
        OP_MAILBOX_WAIT => mailbox::handle_wait(handle),
        // ...
        _ => Box::pin(core::future::ready(SyscallResult::err(-1))),
    })
}

/// Poll a future once. If Ready, copy out any WriteBack and return.
/// If Pending, store as PendingSyscall and yield.
fn poll_and_dispatch(future: SyscallFuture, return_rip: usize, user_rsp: usize) -> isize {
    let pid = scheduler::current_process_id();
    let waker = ProcessWaker::new(pid).into_waker();
    let mut cx = core::task::Context::from_waker(&waker);
    match Pin::as_mut(&mut future).poll(&mut cx) {
        Poll::Ready(result) => {
            if let Some(wb) = result.writeback {
                let ua = unsafe { UserAccess::new() };
                // Page table still active — safe to write
                let _ = ua.write(wb.dst, &wb.data);
            }
            result.code
        }
        Poll::Pending => {
            scheduler::with_current_process(|proc| {
                proc.set_pending_syscall(PendingSyscall::new(future));
            });
            unsafe {
                scheduler::yield_current(
                    VirtAddr::new(return_rip as u64),
                    VirtAddr::new(user_rsp as u64),
                );
            }
        }
    }
}
```

No handler sees `SyscallContext`, `return_rip`, or `user_rsp`. Those are only used by the top-level. Handlers that read from userspace take `&UserAccess` and return `Result<SyscallFuture, BadUserPointer>`. Handlers that don't touch userspace return `SyscallFuture` directly (wrapped in `Ok` by `build_future`).

### Handler signatures

Handlers take only their arguments and return a boxed future. Three patterns:

**Synchronous** — wraps result in `ready()`:
```rust
pub fn handle_close(handle: u32) -> Pin<Box<dyn Future<Output = isize> + Send>> {
    let result = scheduler::with_current_process(|proc| { ... });
    Box::pin(core::future::ready(result))
}
```

**Blocking** — uses `poll_fn`, returns `Pending` until ready:
```rust
pub fn handle_wait(handle_id: u32) -> Pin<Box<dyn Future<Output = isize> + Send>> {
    let resource = scheduler::with_current_process(|proc| {
        proc.handles().get(handle_id).map(|h| h.resource_arc())
    });
    Box::pin(poll_fn(move |_cx| {
        let Some(ref resource) = resource else { return Poll::Ready(-1) };
        let process_iface = resource.as_process().unwrap();
        match process_iface.exit_code() {
            Some(code) => Poll::Ready(code as isize),
            None => {
                process_iface.waker().set_waiting(scheduler::current_process_id());
                Poll::Pending
            }
        }
    }))
}
```

**Async I/O** — existing `async move {}` blocks, returned directly:
```rust
pub fn handle_open(...) -> Pin<Box<dyn Future<Output = isize> + Send>> {
    Box::pin(async move {
        let resource = resource::open(&uri).await;
        // ...
    })
}
```

### Waker bridge

Device `Waker`s (`set_waiting`/`wake`) and Rust's `core::task::Waker` (via `ProcessWaker`) both converge on `scheduler::wake_process(pid)`. When a device signals, the process becomes Runnable, the scheduler polls the future, and the future retries the operation. Futures call `device_waker.set_waiting(pid)` on each `Pending` return to re-register.

### `poll_fn` helper

```rust
pub struct PollFn<F>(F);

impl<F> Future for PollFn<F>
where F: FnMut(&mut Context<'_>) -> Poll<isize> + Send + Unpin
{
    type Output = isize;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<isize> {
        (self.0)(cx)
    }
}

pub fn poll_fn<F>(f: F) -> PollFn<F>
where F: FnMut(&mut Context<'_>) -> Poll<isize> + Send + Unpin
{
    PollFn(f)
}
```

## Handler migration summary

### Already-async handlers (currently use `yield_for_async`)

These currently build a `Box::pin(async { ... })`, store it as `PendingSyscall`, and call `ctx.yield_for_async()`. Migration: just return the future instead.

- `environment::handle_open` — return existing future
- `environment::handle_spawn` — return existing future
- `environment::handle_opendir` — return existing future
- `environment::handle_mount` — return existing future
- `file::handle_read` (VFS path) — return existing future
- `file::handle_write` (VFS path) — return existing future
- `file::handle_seek` (SEEK_END) — return existing future
- `file::handle_stat` — return existing future
- `buffer::handle_read_buffer` — return existing future
- `buffer::handle_write_buffer` — return existing future
- `surface::handle_flush` (window path) — return existing future

### Currently-blocking handlers (use `block_on`)

These need new futures using `poll_fn`:

- `channel::handle_send` — try `channel.send()`, pend on `QueueFull`
- `channel::handle_recv` — try `channel.recv()`, pend on `QueueEmpty`
- `file::handle_read_sync` — try `event_source.poll()`, pend if no event
- `mailbox::handle_wait` — try `mailbox.wait()`, pend if no events
- `process::handle_wait` — try `exit_code()`, pend if still running

### Synchronous handlers

Wrap in `Box::pin(async { ... })` or return `Box::pin(core::future::ready(result))`:

- `file::handle_close`, `handle_readdir`
- `process::handle_get_pid`, `handle_signal`, `handle_brk`
- `environment::handle_log`, `handle_time`
- `buffer::handle_alloc`, `handle_resize`, `handle_free`
- `surface::handle_info`, `handle_blit`, `handle_fill`, `handle_update_params`
- `mailbox::handle_create`, `handle_poll`
- `channel::handle_create`

### Diverging handlers (stay special-cased)

- `process::handle_yield` — calls `scheduler::yield_current` directly
- `process::handle_exit` — calls `scheduler::remove_process` + `exec_next_runnable`

## Userspace memory safety

### Problem

Some futures currently access userspace memory directly via raw pointers (`buf_ptr as *mut u8`). This requires the process's page table to be active during polling — a runtime invariant with no compile-time enforcement.

### Solution: `UserSlice` + `!Send` `UserAccess`

Introduce types that make it a compile error to access userspace memory inside a `Send` future, and a runtime error to dereference an invalid pointer.

```rust
// panda-kernel/src/syscall/user_ptr.rs

/// A region of userspace memory. Stores address and length but cannot be
/// dereferenced directly — you need a UserAccess token.
/// UserSlice is Send + Copy, so it can safely be captured in futures
/// (it's just two integers).
#[derive(Clone, Copy)]
pub struct UserSlice {
    addr: usize,
    len: usize,
}

impl UserSlice {
    pub fn new(addr: usize, len: usize) -> Self {
        Self { addr, len }
    }

    pub fn len(&self) -> usize { self.len }
}

/// Proof that the current process's page table is active.
/// Not Send — cannot be captured in a Send future.
///
/// All reads/writes validate that the pointer falls within the
/// userspace address range (lower canonical half: 0 to 0x7fff_ffff_ffff)
/// before accessing memory.
pub struct UserAccess(());

impl !Send for UserAccess {}

/// Upper bound of userspace addresses (lower canonical half).
const USER_ADDR_MAX: usize = 0x0000_7fff_ffff_ffff;

/// Errors that can occur during syscall setup (before the future runs).
/// Handlers return these via ? to bail out early.
#[derive(Debug)]
pub enum SyscallError {
    BadUserPointer,
    InvalidHandle,
    // Future variants as needed
}

impl UserAccess {
    /// # Safety
    /// Caller must ensure the current process's page table is active.
    pub(crate) unsafe fn new() -> Self { Self(()) }

    /// Validate that a UserSlice falls entirely within userspace.
    fn validate(&self, slice: UserSlice) -> Result<(), SyscallError> {
        if slice.len == 0 {
            return Ok(());
        }
        let end = slice.addr.checked_add(slice.len)
            .ok_or(SyscallError::BadUserPointer)?;
        if end - 1 > USER_ADDR_MAX {
            return Err(SyscallError::BadUserPointer);
        }
        Ok(())
    }

    /// Copy data from userspace into a kernel Vec.
    pub fn read(&self, src: UserSlice) -> Result<Vec<u8>, SyscallError> {
        self.validate(src)?;
        let slice = unsafe { core::slice::from_raw_parts(src.addr as *const u8, src.len) };
        Ok(slice.to_vec())
    }

    /// Copy data from kernel into userspace. Returns bytes written.
    pub fn write(&self, dst: UserSlice, data: &[u8]) -> Result<usize, SyscallError> {
        self.validate(dst)?;
        let slice = unsafe { core::slice::from_raw_parts_mut(dst.addr as *mut u8, dst.len) };
        let n = data.len().min(slice.len());
        slice[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }

    /// Read a Copy struct from userspace.
    pub fn read_struct<T: Copy>(&self, addr: usize) -> Result<T, SyscallError> {
        let slice = UserSlice::new(addr, core::mem::size_of::<T>());
        self.validate(slice)?;
        Ok(unsafe { core::ptr::read(addr as *const T) })
    }

    /// Write a Copy struct to userspace.
    pub fn write_struct<T: Copy>(&self, addr: usize, value: &T) -> Result<(), SyscallError> {
        let slice = UserSlice::new(addr, core::mem::size_of::<T>());
        self.validate(slice)?;
        unsafe { core::ptr::write(addr as *mut T, *value) };
        Ok(())
    }
}
```

**Three layers of enforcement:**

1. **Compile-time (`!Send`)**: If a future tries to capture `UserAccess`, it won't be `Send`, and storing it as `Pin<Box<dyn Future + Send>>` fails at compile time.

2. **Compile-time (`#![deny(unsafe_code)]`)**: Each syscall handler module (`channel.rs`, `file.rs`, `mailbox.rs`, `process.rs`, `environment.rs`, `buffer.rs`, `surface.rs`) gets `#![deny(unsafe_code)]`. Handlers cannot use `unsafe` at all, so they cannot dereference raw pointers. Only `mod.rs` (top-level dispatch) and `user_ptr.rs` (the `UserAccess` implementation) permit `unsafe`.

3. **Runtime (address validation)**: `read()` and `write()` return `Result<_, SyscallError>`, validating that the entire range falls within `0..=0x0000_7fff_ffff_ffff` (the lower canonical half). Handlers propagate failures with `?`.

Additionally, `UserSlice` has private fields — handlers cannot extract the raw address. The only way to access userspace memory is through `UserAccess`, which is only available outside of futures.

### How handlers use it

Handlers that need to read from userspace receive a `&UserAccess` and copy data before building their future. Handlers that need to write to userspace capture a `UserSlice` (just two integers, `Send`-safe) in their future and return the data via `WriteBack` for the top-level to copy out.

**Example: channel send (reads from userspace)**
```rust
pub fn handle_send(
    ua: &UserAccess,       // proves page table is active
    handle: u32,
    buf: UserSlice,        // opaque address+len
    flags: usize,
) -> Result<SyscallFuture, SyscallError> {
    // Copy message from userspace NOW, while page table is active.
    let msg = ua.read(buf)?;
    let resource = get_channel(handle);

    // Future only captures msg (Vec<u8>) and resource (Arc).
    // ua is NOT captured — compiler enforces this.
    Box::pin(poll_fn(move |_cx| {
        let Some(ref resource) = resource else { return Poll::Ready(SyscallResult::err(-1)) };
        let channel = resource.as_channel().unwrap();
        match channel.send(&msg) {
            Ok(()) => Poll::Ready(SyscallResult::ok(0)),
            Err(ChannelError::QueueFull) => {
                channel.waker().set_waiting(scheduler::current_process_id());
                Poll::Pending
            }
            Err(ChannelError::PeerClosed) => Poll::Ready(SyscallResult::err(-3)),
            Err(_) => Poll::Ready(SyscallResult::err(-4)),
        }
    }))
}
```

**Example: channel recv (writes to userspace)**
```rust
pub fn handle_recv(
    handle: u32,
    dst: UserSlice,        // captured in future (just two integers)
    flags: usize,
) -> Pin<Box<dyn Future<Output = SyscallResult> + Send>> {
    // No ua needed — not reading from userspace yet
    let resource = get_channel(handle);

    Box::pin(poll_fn(move |_cx| {
        let Some(ref resource) = resource else { return Poll::Ready(SyscallResult::err(-1)) };
        let channel = resource.as_channel().unwrap();
        let mut kernel_buf = vec![0u8; dst.len()];
        match channel.recv(&mut kernel_buf) {
            Ok(len) => {
                kernel_buf.truncate(len);
                // Return data + destination for top-level to copy out
                Poll::Ready(SyscallResult::write_back(len as isize, kernel_buf, dst))
            }
            Err(ChannelError::QueueEmpty) => {
                channel.waker().set_waiting(scheduler::current_process_id());
                Poll::Pending
            }
            Err(ChannelError::PeerClosed) => Poll::Ready(SyscallResult::err(-3)),
            Err(_) => Poll::Ready(SyscallResult::err(-4)),
        }
    }))
}
```

**Example: what the compiler catches**
```rust
pub fn handle_bad(ua: &UserAccess, ...) -> Result<SyscallFuture, SyscallError> {
    Ok(Box::pin(async move {
        let data = ua.read(buf);  // COMPILE ERROR: UserAccess is !Send,
                                   // future containing it can't be Send
    }))
}
```

### Top-level flow with copy-in/copy-out

The top-level `syscall_handler` creates a `UserAccess` token (it runs with the correct page table). Handlers that need userspace data receive it pre-copied, and return kernel-side results. The top-level copies results out.

For futures that produce data to write back to userspace, the return type becomes:

```rust
pub struct SyscallResult {
    pub code: isize,
    pub writeback: Option<WriteBack>,
}

pub struct WriteBack {
    pub data: Vec<u8>,
    pub dst: UserSlice,
}
```

The top-level does the copy-out after `Poll::Ready`:

```rust
match Pin::as_mut(&mut future).poll(&mut cx) {
    Poll::Ready(result) => {
        if let Some(wb) = result.writeback {
            let ua = unsafe { UserAccess::new() };
            ua.write(wb.dst, &wb.data);
        }
        result.code
    }
    Poll::Pending => { /* store as PendingSyscall, yield */ }
}
```

The scheduler's `exec_next_runnable` does the same after polling a pending future — switch page table, poll, copy-out on Ready.

### Affected futures

**Reads from userspace (copy-in before future):**
- `channel::handle_send` — copy message bytes into `Vec<u8>` before building future
- `file::handle_write_vfs` — copy write data into `Vec<u8>` before building future
- `file::handle_write_sync` — synchronous, done before future (already works)

**Writes to userspace (copy-out after future):**
- `channel::handle_recv` — future receives into kernel `Vec<u8>`, top-level copies out
- `file::handle_read_vfs` — future reads into kernel `Vec<u8>`, top-level copies out
- `file::handle_read_sync` — future returns event bytes as `Vec<u8>`, top-level copies out
- `file::handle_stat` — future returns stat data, top-level copies out

**Already kernel-side (no change needed):**
- `buffer::handle_read_buffer` / `handle_write_buffer` — use `SharedBuffer` (kernel memory)
- `environment::handle_open` / `handle_spawn` — copy path strings before future
- `surface::handle_flush` — works through kernel objects

### Synchronous handlers

Synchronous handlers that access userspace memory do so before returning their (immediately-ready) future. They can use `UserAccess` directly since they run in the syscall handler with the correct page table. The `UserAccess` is dropped before the future is returned.

## Cleanup after migration

Remove:
- `SyscallContext` struct, `block_on()`, and `yield_for_async()` — no handler uses them
- `CalleeSavedRegs` from Rust side — no handler reads callee-saved registers
- `SavedState::for_syscall_restart()` — no longer needed without re-execution
- `scheduler::block_current_on()` — no callers remain
- `callee_saved` parameter from `syscall_handler` — assembly still pushes/pops them (ABI), but Rust doesn't need the pointer

Keep:
- `SavedState::from_interrupt()` (preemption)
- Device `Waker` struct (futures use `set_waiting`/`wake`)
- `scheduler::yield_current()` (used by `handle_yield` and the top-level Pending path)
- `CalleeSavedRegs` push/pop in assembly entry (ABI requirement, removing is a follow-up)

## Implementation order

1. Add `UserSlice`, `UserAccess`, `SyscallResult`, `WriteBack` types in new `syscall/user_ptr.rs`
2. Add `poll_fn` helper in `syscall/mod.rs`
3. Restructure top-level dispatch: split diverging ops (yield, exit), change `handle_send` to return futures, add poll-once logic with copy-out
4. Migrate `block_on` handlers to return futures with `poll_fn` (process wait, mailbox wait, channel send/recv, file read_sync) — channel send copies data in, channel recv and file read_sync return `WriteBack`
5. Migrate `yield_for_async` handlers to return futures directly — VFS read/write switch to kernel bounce buffers and `WriteBack`
6. Migrate synchronous handlers to return `Box::pin(ready(...))`
7. Update scheduler `exec_next_runnable` to handle `SyscallResult` (copy-out after polling)
8. Remove `SyscallContext`, `block_on`, `yield_for_async`, `block_current_on`, `for_syscall_restart`
9. Audit: grep for `from_raw_parts` in `syscall/` to confirm no remaining direct userspace access in futures
10. Update `docs/KERNEL_INTERNALS.md`

## Files modified

- **New: `panda-kernel/src/syscall/user_ptr.rs`** — `UserSlice`, `UserAccess`, `SyscallResult`, `WriteBack`
- `panda-kernel/src/syscall/mod.rs` — top-level dispatch with poll-once and copy-out, `poll_fn`, remove `SyscallContext`
- `panda-kernel/src/syscall/channel.rs` — return futures, copy-in for send, `WriteBack` for recv
- `panda-kernel/src/syscall/file.rs` — return futures, kernel bounce buffers for VFS read/write, `WriteBack` for reads
- `panda-kernel/src/syscall/mailbox.rs` — return futures
- `panda-kernel/src/syscall/process.rs` — return futures (wait), keep diverging (exit, yield)
- `panda-kernel/src/syscall/environment.rs` — return futures instead of yield_for_async
- `panda-kernel/src/syscall/buffer.rs` — return futures instead of yield_for_async
- `panda-kernel/src/syscall/surface.rs` — return futures instead of yield_for_async
- `panda-kernel/src/process/state.rs` — remove `for_syscall_restart`
- `panda-kernel/src/scheduler/mod.rs` — remove `block_current_on`, update polling to handle `SyscallResult`
- `docs/KERNEL_INTERNALS.md` — update syscall documentation

## Verification

1. `make build` — must compile
2. `make test` — all existing test suites pass
3. Key tests: anything using channels, mailbox wait, keyboard input, process wait
4. Manual: run terminal, verify keyboard input works
5. Manual: run a pipeline (channel send/recv)
