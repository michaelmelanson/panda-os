//! Environment operations.
//!
//! The environment handle provides access to system-level operations
//! like opening files, spawning processes, and logging.

use crate::error::{self, Result};
use crate::handle::Handle;
use crate::process::ChildBuilder;
use crate::sys;
use panda_abi::*;

/// Open a file by path.
///
/// Returns a file handle on success.
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
pub fn open(path: &str, mailbox: u32, event_mask: u32) -> Result<Handle> {
    error::from_syscall_handle(sys::env::open(path, mailbox, event_mask))
}

/// Spawn a new process from an executable path.
///
/// Returns a raw handle for manual process management.
/// For RAII-managed child processes, use `Child::spawn()` or `Child::builder()`.
///
/// # Example
/// ```
/// // Simple spawn returning raw handle
/// let handle = environment::spawn("file:/initrd/hello")?;
///
/// // For more options, use Child::builder():
/// let child = Child::builder("file:/initrd/cat")
///     .args(&["cat", "file.txt"])
///     .mailbox(mb.handle().as_raw(), EVENT_CHANNEL_READABLE)
///     .spawn()?;
/// ```
pub fn spawn(path: &str) -> Result<Handle> {
    ChildBuilder::new(path).spawn_handle()
}

/// Log a message to the system console.
#[inline(always)]
pub fn log(msg: &str) {
    sys::env::log(msg);
}

/// Get the current system time.
///
/// Returns a timestamp, or negative error code.
#[inline(always)]
pub fn time() -> isize {
    sys::env::time()
}

/// Open a directory for iteration.
///
/// Returns a directory handle on success.
#[inline(always)]
pub fn opendir(path: &str) -> Result<Handle> {
    error::from_syscall_handle(sys::env::opendir(path))
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

/// Mount a filesystem.
///
/// # Arguments
/// * `fstype` - Filesystem type (e.g., "ext2")
/// * `mountpoint` - Path where the filesystem should be mounted (e.g., "/mnt")
#[inline(always)]
pub fn mount(fstype: &str, mountpoint: &str) -> Result<()> {
    error::from_syscall_unit(sys::env::mount(fstype, mountpoint))
}

/// Check if a file or directory exists at the given path.
///
/// Returns Ok(FileStat) if the path exists, Err otherwise.
/// This is useful for checking file existence before opening.
pub fn stat(path: &str) -> Result<FileStat> {
    // Open the file to get a handle, then stat it
    let handle = open(path, 0, 0)?;
    let mut stat_buf = FileStat {
        size: 0,
        is_dir: false,
    };
    let result = crate::file::stat(handle, &mut stat_buf);
    crate::file::close(handle);
    error::from_syscall_unit(result)?;
    Ok(stat_buf)
}
