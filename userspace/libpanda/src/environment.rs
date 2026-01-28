//! Environment operations.
//!
//! The environment handle provides access to system-level operations
//! like opening files, spawning processes, and logging.

use crate::handle::Handle;
use crate::sys;
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
    let result = sys::env::open(path, mailbox, event_mask);
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
    spawn_with_env(path, args, &[], mailbox, event_mask)
}

/// Spawn a new process with explicit environment variables.
///
/// Like `spawn`, but allows specifying environment variables to pass to the child.
/// If `env` is empty, the current process's environment is inherited.
/// If `env` is non-empty, it replaces the inherited environment.
pub fn spawn_with_env(
    path: &str,
    args: &[&str],
    env: &[(&str, &str)],
    mailbox: u32,
    event_mask: u32,
) -> Result<Handle, isize> {
    spawn_full(path, args, env, mailbox, event_mask, 0, 0)
}

/// Internal spawn implementation with all options.
fn spawn_full(
    path: &str,
    args: &[&str],
    env: &[(&str, &str)],
    mailbox: u32,
    event_mask: u32,
    stdin: u32,
    stdout: u32,
) -> Result<Handle, isize> {
    let result = sys::env::spawn(path, mailbox, event_mask, stdin, stdout);
    if result < 0 {
        return Err(result);
    }

    let handle = Handle::from(result as u32);

    // Determine environment to send
    // If explicit env provided, use it; otherwise inherit current environment
    let inherited_env: alloc::vec::Vec<(alloc::string::String, alloc::string::String)>;
    let env_refs: alloc::vec::Vec<(&str, &str)>;

    if env.is_empty() {
        // Inherit current process environment
        inherited_env = crate::env::vars();
        env_refs = inherited_env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
    } else {
        inherited_env = alloc::vec::Vec::new();
        let _ = &inherited_env; // Suppress unused warning
        env_refs = env.to_vec();
    }

    // Send startup message with args and environment
    let mut buf = [0u8; MAX_MESSAGE_SIZE];
    if let Ok(len) = crate::startup::encode_with_env(args, &env_refs, &mut buf) {
        // Best effort - ignore send errors (child may not be ready or doesn't care)
        let _ = crate::channel::send(handle, &buf[..len]);
    }

    Ok(handle)
}

/// Spawn a new process with explicit stdin/stdout redirection.
///
/// Like `spawn`, but allows specifying handles for the child's STDIN and STDOUT.
/// Pass 0 for stdin/stdout to leave them unconnected.
///
/// This is used for setting up pipelines where processes communicate via
/// channels rather than through the parent.
///
/// # Example
/// ```
/// // Create a channel pair for pipeline
/// let (read_end, write_end) = channel::create_pair();
///
/// // Spawn producer with stdout connected to write_end
/// let producer = environment::spawn_with_stdio(
///     "file:/mnt/ls", &["ls"],
///     mailbox.handle().as_raw(), EVENT_PROCESS_EXITED,
///     0, write_end.as_raw(),
/// )?;
///
/// // Spawn consumer with stdin connected to read_end
/// let consumer = environment::spawn_with_stdio(
///     "file:/mnt/grep", &["grep", "foo"],
///     mailbox.handle().as_raw(), EVENT_PROCESS_EXITED,
///     read_end.as_raw(), 0,
/// )?;
/// ```
pub fn spawn_with_stdio(
    path: &str,
    args: &[&str],
    mailbox: u32,
    event_mask: u32,
    stdin: u32,
    stdout: u32,
) -> Result<Handle, isize> {
    spawn_full(path, args, &[], mailbox, event_mask, stdin, stdout)
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
/// Returns a directory handle on success, or error code.
#[inline(always)]
pub fn opendir(path: &str) -> Result<Handle, isize> {
    let result = sys::env::opendir(path);
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

/// Mount a filesystem.
///
/// # Arguments
/// * `fstype` - Filesystem type (e.g., "ext2")
/// * `mountpoint` - Path where the filesystem should be mounted (e.g., "/mnt")
///
/// Returns 0 on success, or negative error code.
#[inline(always)]
pub fn mount(fstype: &str, mountpoint: &str) -> Result<(), isize> {
    let result = sys::env::mount(fstype, mountpoint);
    if result < 0 { Err(result) } else { Ok(()) }
}

/// Check if a file or directory exists at the given path.
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
