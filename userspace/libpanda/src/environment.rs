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

/// Parameters for spawning a new process.
///
/// Use the builder pattern to configure spawn options, then call `spawn()`.
///
/// # Example
/// ```
/// // Simple spawn
/// let child = SpawnParams::new("file:/initrd/hello").spawn()?;
///
/// // Spawn with args and mailbox
/// let child = SpawnParams::new("file:/initrd/cat")
///     .args(&["cat", "/mnt/file.txt"])
///     .mailbox(mb.handle().as_raw(), EVENT_CHANNEL_READABLE)
///     .spawn()?;
///
/// // Pipeline with stdin/stdout redirection
/// let child = SpawnParams::new("file:/mnt/grep")
///     .args(&["grep", "foo"])
///     .stdin(read_end.as_raw())
///     .spawn()?;
/// ```
pub struct SpawnParams<'a> {
    path: &'a str,
    args: &'a [&'a str],
    env: &'a [(&'a str, &'a str)],
    mailbox: u32,
    event_mask: u32,
    stdin: u32,
    stdout: u32,
}

impl<'a> SpawnParams<'a> {
    /// Create spawn parameters for the given executable path.
    pub fn new(path: &'a str) -> Self {
        Self {
            path,
            args: &[],
            env: &[],
            mailbox: 0,
            event_mask: 0,
            stdin: 0,
            stdout: 0,
        }
    }

    /// Set command-line arguments.
    ///
    /// The first argument should typically be the program name.
    pub fn args(mut self, args: &'a [&'a str]) -> Self {
        self.args = args;
        self
    }

    /// Set environment variables.
    ///
    /// If not set or empty, the child inherits the parent's environment.
    /// If set, these variables replace the inherited environment.
    pub fn env(mut self, env: &'a [(&'a str, &'a str)]) -> Self {
        self.env = env;
        self
    }

    /// Attach to a mailbox for event notifications.
    ///
    /// # Arguments
    /// * `mailbox` - Handle to the mailbox (use `mailbox.handle().as_raw()`)
    /// * `event_mask` - Events to listen for (e.g., `EVENT_CHANNEL_READABLE`)
    pub fn mailbox(mut self, mailbox: u32, event_mask: u32) -> Self {
        self.mailbox = mailbox;
        self.event_mask = event_mask;
        self
    }

    /// Set the child's stdin to read from the given handle.
    ///
    /// The handle should be a channel endpoint. If not set, the child
    /// uses HANDLE_PARENT for input.
    pub fn stdin(mut self, handle: u32) -> Self {
        self.stdin = handle;
        self
    }

    /// Set the child's stdout to write to the given handle.
    ///
    /// The handle should be a channel endpoint. If not set, the child
    /// uses HANDLE_PARENT for output.
    pub fn stdout(mut self, handle: u32) -> Self {
        self.stdout = handle;
        self
    }

    /// Spawn the process with the configured parameters.
    pub fn spawn(self) -> Result<Handle, isize> {
        let result = sys::env::spawn(
            self.path,
            self.mailbox,
            self.event_mask,
            self.stdin,
            self.stdout,
        );
        if result < 0 {
            return Err(result);
        }

        let handle = Handle::from(result as u32);

        // Determine environment to send
        // If explicit env provided, use it; otherwise inherit current environment
        let inherited_env: alloc::vec::Vec<(alloc::string::String, alloc::string::String)>;
        let env_refs: alloc::vec::Vec<(&str, &str)>;

        if self.env.is_empty() {
            // Inherit current process environment
            inherited_env = crate::env::vars();
            env_refs = inherited_env
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
        } else {
            inherited_env = alloc::vec::Vec::new();
            let _ = &inherited_env; // Suppress unused warning
            env_refs = self.env.to_vec();
        }

        // Send startup message with args and environment
        let mut buf = [0u8; MAX_MESSAGE_SIZE];
        if let Ok(len) = crate::startup::encode_with_env(self.args, &env_refs, &mut buf) {
            // Best effort - ignore send errors (child may not be ready or doesn't care)
            let _ = crate::channel::send(handle, &buf[..len]);
        }

        Ok(handle)
    }
}

/// Spawn a new process from an executable path.
///
/// This is a convenience function. For more options, use `SpawnParams`.
///
/// # Example
/// ```
/// // Simple spawn
/// let child = environment::spawn("file:/initrd/hello")?;
///
/// // For more options, use SpawnParams:
/// let child = SpawnParams::new("file:/initrd/cat")
///     .args(&["cat", "file.txt"])
///     .mailbox(mb.handle().as_raw(), EVENT_CHANNEL_READABLE)
///     .spawn()?;
/// ```
pub fn spawn(path: &str) -> Result<Handle, isize> {
    SpawnParams::new(path).spawn()
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
