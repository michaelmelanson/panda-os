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
/// # Examples
///
/// Simple open without mailbox:
/// ```no_run
/// use libpanda::environment;
///
/// let file = environment::open("file:/initrd/hello.txt", 0, 0).unwrap();
/// ```
///
/// Open with mailbox attachment for events:
/// ```no_run
/// use libpanda::environment;
/// use libpanda::mailbox::Mailbox;
///
/// let mailbox = Mailbox::default();
/// let file = environment::open(
///     "keyboard:/pci/00:03.0",
///     mailbox.handle().as_raw(),
///     panda_abi::EVENT_KEYBOARD_KEY,
/// ).unwrap();
/// ```
#[inline(always)]
pub fn open(path: &str, mailbox: u64, event_mask: u32) -> Result<Handle> {
    error::from_syscall_handle(sys::env::open(path, mailbox, event_mask))
}

/// Spawn a new process from an executable path.
///
/// Returns a raw handle for manual process management.
/// For RAII-managed child processes, use `Child::spawn()` or `Child::builder()`.
///
/// # Examples
///
/// Simple spawn returning raw handle:
/// ```no_run
/// use libpanda::environment;
///
/// let handle = environment::spawn("file:/initrd/hello").unwrap();
/// ```
///
/// For more options, use `Child::builder()`:
/// ```no_run
/// use libpanda::process::Child;
/// use libpanda::mailbox::Mailbox;
///
/// let mb = Mailbox::default();
/// let child = Child::builder("file:/initrd/cat")
///     .args(&["cat", "file.txt"])
///     .mailbox(mb.handle(), panda_abi::EVENT_CHANNEL_READABLE)
///     .spawn()
///     .unwrap();
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

/// Create a new file at the given path.
///
/// Returns a file handle on success. The file is opened for reading and writing.
///
/// # Arguments
/// * `path` - URI of the file to create (e.g., "file:/mnt/newfile.txt")
/// * `mode` - File permissions (e.g., 0o644)
/// * `mailbox` - Mailbox handle for event notifications (0 = none)
#[inline(always)]
pub fn create(path: &str, mode: u16, mailbox: u64) -> Result<Handle> {
    error::from_syscall_handle(sys::env::create(path, mode, mailbox))
}

/// Unlink (delete) a file at the given path.
///
/// Removes the directory entry and, if no other links remain, frees the
/// file's data blocks and inode.
///
/// # Arguments
/// * `path` - URI of the file to unlink (e.g., "file:/mnt/file.txt")
#[inline(always)]
pub fn unlink(path: &str) -> Result<()> {
    error::from_syscall_unit(sys::env::unlink(path))
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
