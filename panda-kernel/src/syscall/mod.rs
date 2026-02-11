//! Syscall handling infrastructure.
//!
//! This module handles the system call interface between userspace and the kernel.
//! It provides:
//! - GDT/TSS setup for privilege transitions
//! - Syscall entry point (via SYSCALL/SYSRET)
//! - Dispatch to operation-specific handlers

mod buffer;
mod channel;
mod directory;
mod entry;
mod environment;
mod file;
pub mod gdt;
mod mailbox;
mod process;
mod surface;
pub(crate) mod user_ptr;

use log::{debug, error};
use x86_64::VirtAddr;

use alloc::boxed::Box;
use alloc::sync::Arc;

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use crate::{resource::VfsFile, scheduler};

/// A future that delegates to a closure on each poll.
///
/// This is the kernel's equivalent of `core::future::poll_fn` (not available in
/// `no_std`). Used by blocking syscall handlers that retry an operation on each
/// poll until it succeeds.
pub(crate) struct PollFn<F>(F);

impl<F> Future for PollFn<F>
where
    F: FnMut(&mut Context<'_>) -> Poll<user_ptr::SyscallResult> + Send + Unpin,
{
    type Output = user_ptr::SyscallResult;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<user_ptr::SyscallResult> {
        (self.0)(cx)
    }
}

/// Create a future from a closure that is called on each poll.
pub(crate) fn poll_fn<F>(f: F) -> PollFn<F>
where
    F: FnMut(&mut Context<'_>) -> Poll<user_ptr::SyscallResult> + Send + Unpin,
{
    PollFn(f)
}

/// Wrapper to allow holding an Arc<dyn Resource> as Arc<dyn VfsFile>.
///
/// Used by syscall handlers to convert Resource handles to VfsFile trait objects.
pub(crate) struct VfsFileWrapper(pub Arc<dyn crate::resource::Resource>);

impl VfsFile for VfsFileWrapper {
    fn file(&self) -> &spinning_top::Spinlock<Box<dyn crate::vfs::File>> {
        self.0.as_vfs_file().unwrap().file()
    }
}

// Safety: VfsFileWrapper just holds an Arc which is Send+Sync
unsafe impl Send for VfsFileWrapper {}
unsafe impl Sync for VfsFileWrapper {}

/// Callee-saved registers that must be preserved across syscalls.
/// These are saved by syscall_entry and passed to syscall_handler for use
/// when a process blocks and needs to restore full state on resume.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct CalleeSavedRegs {
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
}

/// Get the user code segment selector. Must be called after init().
pub fn user_code_selector() -> u16 {
    gdt::user_code_selector()
}

/// Initialize syscall infrastructure (GDT, TSS, MSRs).
pub fn init() {
    gdt::init();
    entry::init();
}

/// Main syscall handler called from entry.rs.
///
/// This is called from the naked syscall_entry function with all registers saved.
/// All non-diverging syscall handlers return a future, which is polled once here.
/// If the future is immediately ready, the result is returned to userspace.
/// If the future is pending, it is stored as a PendingSyscall and the process yields.
#[allow(clippy::too_many_arguments)]
extern "sysv64" fn syscall_handler(
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    code: usize,
    return_rip: usize,
    user_rsp: usize,
    callee_saved: *const CalleeSavedRegs,
) -> isize {
    // Disable interrupts for the entire syscall to prevent race conditions
    let flags = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::disable();

    let result = {
        debug!("SYSCALL: code={code:X}, args: {arg0:X}, {arg1:X}, {arg2:X}, {arg3:X}");

        if code != panda_abi::SYSCALL_SEND {
            -1
        } else {
            let handle = arg0 as u64;
            let operation = arg1 as u32;

            // Safety: callee_saved points to registers pushed by syscall_entry
            // on the kernel stack, valid for the duration of this call.
            // Use read_volatile to ensure the copy happens immediately,
            // before the compiler's register allocator overwrites the area.
            let callee_saved = unsafe { core::ptr::read_volatile(callee_saved) };

            // Phase 1: diverging operations that manipulate the scheduler directly
            // and never return a value to userspace. These use unsafe scheduler
            // functions and are kept here rather than in handler modules.
            match operation {
                panda_abi::OP_PROCESS_YIELD => unsafe {
                    scheduler::yield_current(
                        VirtAddr::new(return_rip as u64),
                        VirtAddr::new(user_rsp as u64),
                        callee_saved,
                    );
                },
                panda_abi::OP_PROCESS_EXIT => {
                    let current_pid = scheduler::current_process_id();
                    log::info!(
                        "Process {:?} exiting with code {}",
                        current_pid,
                        arg2 as i32
                    );
                    let process_info = scheduler::with_current_process(|proc| proc.info().clone());
                    scheduler::remove_process(current_pid);
                    process_info.set_exit_code(arg2 as i32);
                    unsafe {
                        scheduler::exec_next_runnable();
                    }
                }
                _ => {}
            }

            // Phase 2: build a future from the handler.
            // UserAccess is created here (page table is active during syscall entry).
            let ua = unsafe { user_ptr::UserAccess::new() };

            let future = build_future(&ua, handle, operation, arg2, arg3, arg4, arg5);

            // ua is dropped here — cannot leak into futures (it's !Send anyway).
            drop(ua);

            // Phase 3: poll the future once and dispatch.
            poll_and_dispatch(future, return_rip, user_rsp, callee_saved)
        }
    };

    // Restore interrupt state before returning to userspace
    if flags {
        x86_64::instructions::interrupts::enable();
    }

    result
}

/// Build a syscall future by dispatching to the appropriate handler.
///
/// Handlers that read from userspace receive `&UserAccess` to copy data in
/// before building their future. The `UserAccess` token is NOT captured in
/// any future (the compiler enforces this since it is `!Send`).
fn build_future(
    ua: &user_ptr::UserAccess,
    handle: u64,
    operation: u32,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
) -> user_ptr::SyscallFuture {
    use panda_abi::*;

    // For handlers that return Result<SyscallFuture, SyscallError>, unwrap
    // the error into an immediate error future.
    let result: Result<user_ptr::SyscallFuture, user_ptr::SyscallError> = match operation {
        // File operations
        OP_FILE_READ => Ok(file::handle_read(ua, handle, arg0, arg1, arg2 as u32)),
        OP_FILE_WRITE => Ok(file::handle_write(ua, handle, arg0, arg1)),
        OP_FILE_SEEK => Ok(file::handle_seek(handle, arg0, arg1)),
        OP_FILE_STAT => Ok(file::handle_stat(handle, arg0)),
        OP_FILE_CLOSE => Ok(file::handle_close(handle)),
        OP_FILE_READDIR => Ok(file::handle_readdir(ua, handle, arg0)),

        // Process operations (yield and exit are handled above as diverging)
        OP_PROCESS_GET_PID => Ok(process::handle_get_pid()),
        OP_PROCESS_WAIT => Ok(process::handle_wait(handle)),
        OP_PROCESS_SIGNAL => Ok(process::handle_signal()),
        OP_PROCESS_BRK => Ok(process::handle_brk(arg0)),

        // Environment operations
        OP_ENVIRONMENT_OPEN => Ok(environment::handle_open(ua, arg0, arg1, arg2, arg3)),
        OP_ENVIRONMENT_SPAWN => Ok(environment::handle_spawn(ua, user_ptr::UserPtr::new(arg0))),
        OP_ENVIRONMENT_LOG => Ok(environment::handle_log(ua, arg0, arg1)),
        OP_ENVIRONMENT_TIME => Ok(environment::handle_time()),
        OP_ENVIRONMENT_OPENDIR => Ok(environment::handle_opendir(ua, arg0, arg1)),
        OP_ENVIRONMENT_MOUNT => Ok(environment::handle_mount(ua, arg0, arg1, arg2, arg3)),

        // Directory operations
        OP_DIRECTORY_CREATE_FILE => Ok(directory::handle_create(ua, handle, arg0, arg1, arg2, arg3)),
        OP_DIRECTORY_UNLINK_FILE => Ok(directory::handle_unlink(ua, handle, arg0, arg1)),
        OP_DIRECTORY_MKDIR => Ok(directory::handle_mkdir(ua, handle, arg0, arg1, arg2)),
        OP_DIRECTORY_RMDIR => Ok(directory::handle_rmdir(ua, handle, arg0, arg1)),

        // Buffer operations
        OP_BUFFER_ALLOC => Ok(buffer::handle_alloc(ua, arg0, arg1)),
        OP_BUFFER_RESIZE => Ok(buffer::handle_resize(ua, handle, arg0, arg1)),
        OP_BUFFER_FREE => Ok(buffer::handle_free(handle)),

        // Buffer-based file operations
        OP_FILE_READ_BUFFER => Ok(buffer::handle_read_buffer(handle, arg0 as u64)),
        OP_FILE_WRITE_BUFFER => Ok(buffer::handle_write_buffer(handle, arg0 as u64, arg1)),

        // Surface operations
        OP_SURFACE_INFO => Ok(surface::handle_info(
            ua,
            handle,
            user_ptr::UserPtr::new(arg0),
        )),
        OP_SURFACE_BLIT => Ok(surface::handle_blit(
            ua,
            handle,
            user_ptr::UserPtr::new(arg0),
        )),
        OP_SURFACE_FILL => Ok(surface::handle_fill(
            ua,
            handle,
            user_ptr::UserPtr::new(arg0),
        )),
        OP_SURFACE_FLUSH => Ok(surface::handle_flush(
            ua,
            handle,
            if arg0 != 0 {
                Some(user_ptr::UserPtr::new(arg0))
            } else {
                None
            },
        )),
        OP_SURFACE_UPDATE_PARAMS => Ok(surface::handle_update_params(
            ua,
            handle,
            user_ptr::UserPtr::new(arg0),
        )),

        // Mailbox operations
        OP_MAILBOX_CREATE => Ok(mailbox::handle_create()),
        OP_MAILBOX_WAIT => Ok(mailbox::handle_wait(ua, handle, arg0)),
        OP_MAILBOX_POLL => Ok(mailbox::handle_poll(ua, handle, arg0)),

        // Channel operations
        OP_CHANNEL_CREATE => Ok(channel::handle_create(ua, arg0)),
        OP_CHANNEL_SEND => channel::handle_send(ua, handle, arg0, arg1, arg2),
        OP_CHANNEL_RECV => Ok(channel::handle_recv(handle, arg0, arg1, arg2)),

        _ => {
            error!("Unknown operation: {:#x}", operation);
            Ok(Box::pin(core::future::ready(user_ptr::SyscallResult::err(
                panda_abi::ErrorCode::NotSupported,
            ))))
        }
    };

    match result {
        Ok(future) => future,
        Err(e) => Box::pin(core::future::ready(user_ptr::SyscallResult::err(
            e.to_error_code(),
        ))),
    }
}

/// Poll a syscall future once. If ready, perform copy-out and return the result code.
/// If pending, store the future as a PendingSyscall and yield to the scheduler.
///
/// When the future is `Pending`, the callee-saved registers are saved so they can
/// be correctly restored when the process resumes (via `return_from_interrupt`).
/// Without this, userspace would see corrupted rbx/rbp/r12-r15 after a blocking
/// syscall — a bug that only manifests in release builds where the optimiser
/// keeps values in callee-saved registers across syscalls.
fn poll_and_dispatch(
    mut future: user_ptr::SyscallFuture,
    return_rip: usize,
    user_rsp: usize,
    callee_saved: CalleeSavedRegs,
) -> isize {
    use crate::process::{PendingSyscall, ProcessWaker};

    let pid = scheduler::current_process_id();
    let waker = ProcessWaker::new(pid).into_waker();
    let mut cx = core::task::Context::from_waker(&waker);

    match future.as_mut().poll(&mut cx) {
        Poll::Ready(result) => {
            // Copy out writeback data if present (page table is still active)
            if let Some(wb) = result.writeback {
                let ua = unsafe { user_ptr::UserAccess::new() };
                let _ = ua.write(wb.dst, &wb.data);
            }
            result.code
        }
        Poll::Pending => {
            // Store the pending future along with the callee-saved registers.
            // When the future completes later, return_from_deferred_syscall
            // restores rbx/rbp/r12-r15 before sysretq — without this,
            // userspace would see corrupted callee-saved registers.
            scheduler::with_current_process(|proc| {
                proc.set_pending_syscall(PendingSyscall::new(future, callee_saved));
            });
            unsafe {
                scheduler::yield_current(
                    VirtAddr::new(return_rip as u64),
                    VirtAddr::new(user_rsp as u64),
                    callee_saved,
                );
            }
        }
    }
}
