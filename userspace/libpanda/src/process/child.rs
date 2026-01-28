//! Child process management.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::handle::Handle;
use crate::ipc::Channel;
use crate::sys;
use panda_abi::MAX_MESSAGE_SIZE;

/// A handle to a spawned child process.
///
/// `Child` provides RAII management of child processes. When dropped, it waits
/// for the child to exit (unless `into_handle()` is called first).
///
/// # Example
///
/// ```
/// // Spawn a child process
/// let mut child = Child::spawn("file:/initrd/hello")?;
///
/// // Communicate via channel
/// child.channel().send(b"message")?;
///
/// // Wait for exit
/// let status = child.wait()?;
/// assert!(status.success());
/// ```
pub struct Child {
    handle: Handle,
    /// Whether we've already waited for the child.
    waited: bool,
}

impl Child {
    /// Spawn a new child process from an executable path.
    ///
    /// This is a convenience wrapper around `ChildBuilder::new(path).spawn()`.
    pub fn spawn(path: &str) -> Result<Self> {
        ChildBuilder::new(path).spawn()
    }

    /// Spawn a new child process with arguments.
    ///
    /// This is a convenience wrapper for common use cases.
    pub fn spawn_with_args(path: &str, args: &[&str]) -> Result<Self> {
        ChildBuilder::new(path).args(args).spawn()
    }

    /// Create a builder for spawning a child process with custom options.
    pub fn builder(path: &str) -> ChildBuilder<'_> {
        ChildBuilder::new(path)
    }

    /// Get the underlying handle.
    ///
    /// This can be used for mailbox operations or low-level control.
    pub fn handle(&self) -> Handle {
        self.handle
    }

    /// Get a channel for communicating with the child.
    ///
    /// The returned channel borrows the handle and won't close it on drop.
    pub fn channel(&self) -> Channel {
        Channel::from_handle_borrowed(self.handle)
    }

    /// Wait for the child process to exit.
    ///
    /// This is a blocking call that waits until the child terminates.
    /// Returns the exit status of the child.
    pub fn wait(&mut self) -> Result<ExitStatus> {
        if self.waited {
            return Err(Error::InvalidArgument);
        }

        let code = sys::process::wait(self.handle);
        self.waited = true;
        Ok(ExitStatus(code))
    }

    /// Send a signal to the child process.
    pub fn signal(&mut self, sig: Signal) -> Result<()> {
        let result = sys::process::signal(self.handle, sig as u32);
        if result < 0 {
            Err(Error::from_code(result))
        } else {
            Ok(())
        }
    }

    /// Kill the child process (send SIGKILL equivalent).
    pub fn kill(&mut self) -> Result<()> {
        self.signal(Signal::Kill)
    }

    /// Consume the Child and return the underlying handle without waiting.
    ///
    /// After calling this, the child process will continue running
    /// independently. You are responsible for managing the handle.
    pub fn into_handle(self) -> Handle {
        let handle = self.handle;
        core::mem::forget(self);
        handle
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if !self.waited {
            // Wait for child to exit to avoid zombies
            let _ = sys::process::wait(self.handle);
        }
    }
}

/// Builder for spawning child processes with custom options.
///
/// # Example
///
/// ```
/// let child = Child::builder("file:/initrd/worker")
///     .args(&["worker", "--verbose"])
///     .mailbox(mailbox.handle(), EVENT_CHANNEL_READABLE)
///     .spawn()?;
/// ```
pub struct ChildBuilder<'a> {
    path: &'a str,
    args: Vec<&'a str>,
    env: Vec<(&'a str, &'a str)>,
    inherit_env: bool,
    mailbox: u32,
    event_mask: u32,
    stdin: u32,
    stdout: u32,
}

impl<'a> ChildBuilder<'a> {
    /// Create a new builder for spawning a process at the given path.
    pub fn new(path: &'a str) -> Self {
        Self {
            path,
            args: Vec::new(),
            env: Vec::new(),
            inherit_env: true,
            mailbox: 0,
            event_mask: 0,
            stdin: 0,
            stdout: 0,
        }
    }

    /// Set the command-line arguments.
    ///
    /// The first argument is conventionally the program name.
    pub fn args(mut self, args: &[&'a str]) -> Self {
        self.args = args.iter().copied().collect();
        self
    }

    /// Add a single argument.
    pub fn arg(mut self, arg: &'a str) -> Self {
        self.args.push(arg);
        self
    }

    /// Set an environment variable for the child.
    ///
    /// By default, the child inherits the parent's environment. Variables
    /// set via this method are added to (or override) the inherited environment.
    pub fn env(mut self, key: &'a str, value: &'a str) -> Self {
        self.env.push((key, value));
        self
    }

    /// Set multiple environment variables for the child.
    pub fn envs(mut self, vars: &[(&'a str, &'a str)]) -> Self {
        self.env.extend(vars.iter().copied());
        self
    }

    /// Clear the inherited environment.
    ///
    /// After calling this, only environment variables explicitly set via
    /// `env()` or `envs()` will be passed to the child.
    pub fn env_clear(mut self) -> Self {
        self.inherit_env = false;
        self
    }

    /// Attach the child's channel to a mailbox for event notifications.
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
    /// The handle should be a channel endpoint. If not set, the child's
    /// stdin will be invalid and it will use HANDLE_PARENT for input.
    pub fn stdin(mut self, handle: Handle) -> Self {
        self.stdin = handle.as_raw();
        self
    }

    /// Set the child's stdout to write to the given handle.
    ///
    /// The handle should be a channel endpoint. If not set, the child's
    /// stdout will be invalid and it will use HANDLE_PARENT for output.
    pub fn stdout(mut self, handle: Handle) -> Self {
        self.stdout = handle.as_raw();
        self
    }

    /// Spawn the child process.
    pub fn spawn(self) -> Result<Child> {
        let result = sys::env::spawn(
            self.path,
            self.mailbox,
            self.event_mask,
            self.stdin,
            self.stdout,
        );
        if result < 0 {
            return Err(Error::from_code(result));
        }

        let handle = Handle::from(result as u32);

        // Build environment: start with inherited if enabled, then add explicit vars
        let env: Vec<(&str, &str)> = if self.inherit_env {
            // Get current process environment and merge with explicit vars
            let inherited = crate::env::vars();
            let mut env_map: Vec<(&str, &str)> = Vec::new();

            // Add inherited vars (will be overridden by explicit vars with same key)
            for (k, v) in &inherited {
                // Check if this key is overridden by explicit env
                let overridden = self.env.iter().any(|(ek, _)| *ek == k.as_str());
                if !overridden {
                    // Leak strings to get static lifetime - this is fine since we're
                    // about to encode and send them immediately
                    let k_leaked: &'static str =
                        alloc::boxed::Box::leak(k.clone().into_boxed_str());
                    let v_leaked: &'static str =
                        alloc::boxed::Box::leak(v.clone().into_boxed_str());
                    env_map.push((k_leaked, v_leaked));
                }
            }

            // Add explicit vars (these override inherited)
            env_map.extend(self.env.iter().copied());
            env_map
        } else {
            // Only use explicitly set environment variables
            self.env
        };

        // Send startup message with arguments and environment
        let mut buf = [0u8; MAX_MESSAGE_SIZE];
        if let Ok(len) = crate::startup::encode_with_env(&self.args, &env, &mut buf) {
            // Best effort - ignore send errors
            let _ = sys::channel::send_msg(handle, &buf[..len]);
        }

        Ok(Child {
            handle,
            waited: false,
        })
    }
}

/// The exit status of a child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitStatus(i32);

impl ExitStatus {
    /// Returns `true` if the process exited successfully (exit code 0).
    pub fn success(&self) -> bool {
        self.0 == 0
    }

    /// Returns the exit code of the process.
    pub fn code(&self) -> i32 {
        self.0
    }
}

/// Signals that can be sent to a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Signal {
    /// Terminate the process gracefully.
    Term = 0,
    /// Kill the process immediately.
    Kill = 1,
}
