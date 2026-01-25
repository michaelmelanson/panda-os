//! Environment operations using the send-based API
//!
//! The environment handle provides access to system-level operations
//! like opening files, spawning processes, and logging.

use crate::handle::Handle;
use crate::syscall::send;
use panda_abi::*;

/// Open a file by path.
///
/// Returns a file handle on success, or error code.
///
/// To attach the handle to a mailbox for event notifications, pass the
/// mailbox handle and event mask. Pass `(0, 0)` for no mailbox attachment.
///
/// # Example
/// ```
/// // Simple open, no mailbox
/// let file = environment::open("file:/initrd/hello.txt", 0, 0)?;
///
/// // Open with mailbox attachment
/// let file = environment::open(
///     "keyboard:/pci/00:03.0",
///     mailbox.handle().as_raw(),
///     EVENT_KEYBOARD_KEY,
/// )?;
/// ```
#[inline(always)]
pub fn open(path: &str, mailbox: u32, event_mask: u32) -> Result<Handle, isize> {
    let result = send(
        Handle::ENVIRONMENT,
        OP_ENVIRONMENT_OPEN,
        path.as_ptr() as usize,
        path.len(),
        mailbox as usize,
        event_mask as usize,
    );
    if result < 0 {
        Err(result)
    } else {
        Ok(Handle::from(result as u32))
    }
}

/// Spawn a new process from an executable path.
///
/// Returns a spawn handle on success, or error code.
/// The spawn handle provides both a channel to the child and process info.
///
/// If `args` is non-empty, a startup message containing the arguments is
/// sent over the channel to the child process. The first argument should
/// typically be the program name.
///
/// To attach the handle to a mailbox for event notifications, pass the
/// mailbox handle and event mask. Pass `(0, 0)` for no mailbox attachment.
///
/// # Example
/// ```
/// // Simple spawn, no args, no mailbox
/// let child = environment::spawn("file:/initrd/hello", &[], 0, 0)?;
///
/// // Spawn with args
/// let child = environment::spawn(
///     "file:/initrd/cat",
///     &["cat", "/mnt/file.txt"],
///     0, 0,
/// )?;
///
/// // Spawn with mailbox attachment
/// let child = environment::spawn(
///     "file:/initrd/worker",
///     &["worker"],
///     mailbox.handle().as_raw(),
///     EVENT_CHANNEL_READABLE | EVENT_CHANNEL_CLOSED,
/// )?;
/// ```
pub fn spawn(path: &str, args: &[&str], mailbox: u32, event_mask: u32) -> Result<Handle, isize> {
    let result = send(
        Handle::ENVIRONMENT,
        OP_ENVIRONMENT_SPAWN,
        path.as_ptr() as usize,
        path.len(),
        mailbox as usize,
        event_mask as usize,
    );
    if result < 0 {
        return Err(result);
    }

    let handle = Handle::from(result as u32);

    // Send startup message (always, even with empty args)
    let mut buf = [0u8; MAX_MESSAGE_SIZE];
    if let Ok(len) = crate::startup::encode(args, &mut buf) {
        // Best effort - ignore send errors (child may not be ready or doesn't care)
        let _ = crate::channel::send(handle, &buf[..len]);
    }

    Ok(handle)
}

/// Log a message to the system console
#[inline(always)]
pub fn log(msg: &str) {
    let _ = send(
        Handle::ENVIRONMENT,
        OP_ENVIRONMENT_LOG,
        msg.as_ptr() as usize,
        msg.len(),
        0,
        0,
    );
}

/// Get the current system time
///
/// Returns a timestamp, or negative error code
#[inline(always)]
pub fn time() -> isize {
    send(Handle::ENVIRONMENT, OP_ENVIRONMENT_TIME, 0, 0, 0, 0)
}

/// Open a directory for iteration
///
/// Returns a directory handle on success, or error code
#[inline(always)]
pub fn opendir(path: &str) -> Result<Handle, isize> {
    let result = send(
        Handle::ENVIRONMENT,
        OP_ENVIRONMENT_OPENDIR,
        path.as_ptr() as usize,
        path.len(),
        0,
        0,
    );
    if result < 0 {
        Err(result)
    } else {
        Ok(Handle::from(result as u32))
    }
}

/// Signal that the test is ready for screenshot capture.
///
/// This logs a distinctive marker that the test harness watches for.
/// After calling this, the test should halt or loop - the harness will
/// capture a screenshot and terminate QEMU.
///
/// Only used for tests with expected.png files.
#[inline(always)]
pub fn screenshot_ready() {
    log("<<<SCREENSHOT_READY>>>");
}

/// Mount a filesystem
///
/// # Arguments
/// * `fstype` - Filesystem type (e.g., "ext2")
/// * `mountpoint` - Path where the filesystem should be mounted (e.g., "/mnt")
///
/// Returns 0 on success, or negative error code
#[inline(always)]
pub fn mount(fstype: &str, mountpoint: &str) -> Result<(), isize> {
    let result = send(
        Handle::ENVIRONMENT,
        OP_ENVIRONMENT_MOUNT,
        fstype.as_ptr() as usize,
        fstype.len(),
        mountpoint.as_ptr() as usize,
        mountpoint.len(),
    );
    if result < 0 { Err(result) } else { Ok(()) }
}

/// Check if a file or directory exists at the given path
///
/// Returns Ok(FileStat) if the path exists, Err otherwise.
/// This is useful for checking file existence before opening.
pub fn stat(path: &str) -> Result<FileStat, isize> {
    // Open the file to get a handle, then stat it
    let handle = open(path, 0, 0)?;
    let mut stat_buf = FileStat {
        size: 0,
        is_dir: false,
    };
    let result = crate::file::stat(handle, &mut stat_buf);
    crate::file::close(handle);
    if result < 0 {
        Err(result)
    } else {
        Ok(stat_buf)
    }
}
