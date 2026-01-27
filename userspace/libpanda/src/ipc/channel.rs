//! Channel abstraction for message-passing.

use crate::error::{Error, Result};
use crate::handle::Handle;
use crate::sys;

/// A message channel for inter-process communication.
///
/// Channels provide message-based IPC between processes. Messages are atomic
/// byte blocks up to [`MAX_MESSAGE_SIZE`] bytes.
///
/// # Example
///
/// ```
/// // Get channel to parent process
/// if let Some(parent) = Channel::parent() {
///     parent.send(b"hello")?;
///
///     let mut buf = [0u8; 256];
///     let len = parent.recv(&mut buf)?;
///     // Process response...
/// }
/// ```
pub struct Channel {
    handle: Handle,
    /// Whether we own the handle (and should close it on drop).
    owned: bool,
}

impl Channel {
    /// Get the channel to the parent process, if one exists.
    ///
    /// Returns `None` if this process has no parent (e.g., init process).
    pub fn parent() -> Option<Self> {
        Handle::parent().map(|h| Self {
            handle: h,
            owned: false, // Don't close the parent handle
        })
    }

    /// Create a Channel from an existing handle.
    ///
    /// The channel takes ownership of the handle and will close it on drop.
    pub fn from_handle(handle: Handle) -> Self {
        Self {
            handle,
            owned: true,
        }
    }

    /// Create a Channel from a handle without taking ownership.
    ///
    /// The handle will NOT be closed when this Channel is dropped.
    /// Use this when the handle is managed elsewhere (e.g., child process handles).
    pub fn from_handle_borrowed(handle: Handle) -> Self {
        Self {
            handle,
            owned: false,
        }
    }

    /// Get the underlying handle.
    pub fn handle(&self) -> Handle {
        self.handle
    }

    /// Send a message (blocking if queue is full).
    pub fn send(&self, msg: &[u8]) -> Result<()> {
        let result = sys::channel::send_msg(self.handle, msg);
        if result < 0 {
            Err(Error::from_code(result))
        } else {
            Ok(())
        }
    }

    /// Try to send a message (non-blocking).
    ///
    /// Returns `Err(Error::WouldBlock)` if the queue is full.
    pub fn try_send(&self, msg: &[u8]) -> Result<()> {
        let result = sys::channel::try_send_msg(self.handle, msg);
        if result < 0 {
            Err(Error::from_code(result))
        } else {
            Ok(())
        }
    }

    /// Receive a message (blocking if queue is empty).
    ///
    /// Returns the number of bytes received.
    pub fn recv(&self, buf: &mut [u8]) -> Result<usize> {
        let result = sys::channel::recv_msg(self.handle, buf);
        if result < 0 {
            Err(Error::from_code(result))
        } else {
            Ok(result as usize)
        }
    }

    /// Try to receive a message (non-blocking).
    ///
    /// Returns `Ok(Some(len))` if a message was received, `Ok(None)` if the
    /// queue is empty, or `Err` on error.
    pub fn try_recv(&self, buf: &mut [u8]) -> Result<Option<usize>> {
        let result = sys::channel::try_recv_msg(self.handle, buf);
        if result == -1 {
            // Would block - no message available
            Ok(None)
        } else if result < 0 {
            Err(Error::from_code(result))
        } else {
            Ok(Some(result as usize))
        }
    }

    /// Send a request and wait for a response (synchronous call pattern).
    ///
    /// This is a convenience method for the common request/response pattern.
    pub fn call(&self, request: &[u8], response: &mut [u8]) -> Result<usize> {
        self.send(request)?;
        self.recv(response)
    }

    /// Consume the channel and return the underlying handle without closing it.
    pub fn into_handle(self) -> Handle {
        let handle = self.handle;
        core::mem::forget(self);
        handle
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        if self.owned {
            let _ = sys::file::close(self.handle);
        }
    }
}
