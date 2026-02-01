//! Channel abstraction for message-passing.

use crate::error::{self, Result};
use crate::handle::{ChannelHandle, Handle};
use crate::sys;
use panda_abi::ErrorCode;

/// A message channel for inter-process communication.
///
/// Channels provide message-based IPC between processes. Messages are atomic
/// byte blocks up to [`MAX_MESSAGE_SIZE`] bytes.
///
/// # Example
///
/// ```no_run
/// use libpanda::ipc::Channel;
///
/// // Get channel to parent process
/// if let Some(parent) = Channel::parent() {
///     parent.send(b"hello").unwrap();
///
///     let mut buf = [0u8; 256];
///     let len = parent.recv(&mut buf).unwrap();
///     // Process response...
/// }
/// ```
pub struct Channel {
    handle: ChannelHandle,
    /// Whether we own the handle (and should close it on drop).
    owned: bool,
}

impl Channel {
    /// Get the channel to the parent process, if one exists.
    ///
    /// Returns `None` if this process has no parent (e.g., init process).
    pub fn parent() -> Option<Self> {
        ChannelHandle::parent().map(|h| Self {
            handle: h,
            owned: false, // Don't close the parent handle
        })
    }

    /// Create a Channel from an existing untyped handle.
    ///
    /// The channel takes ownership of the handle and will close it on drop.
    /// Returns `None` if the handle is not a channel handle.
    pub fn from_handle(handle: Handle) -> Option<Self> {
        let handle = ChannelHandle::from_raw(handle.as_raw())?;
        Some(Self {
            handle,
            owned: true,
        })
    }

    /// Create a Channel from a typed handle.
    ///
    /// The channel takes ownership of the handle and will close it on drop.
    pub fn from_typed(handle: ChannelHandle) -> Self {
        Self {
            handle,
            owned: true,
        }
    }

    /// Create a Channel from an untyped handle without taking ownership.
    ///
    /// The handle will NOT be closed when this Channel is dropped.
    /// Use this when the handle is managed elsewhere (e.g., child process handles).
    /// Returns `None` if the handle is not a channel handle.
    pub fn from_handle_borrowed(handle: Handle) -> Option<Self> {
        let handle = ChannelHandle::from_raw(handle.as_raw())?;
        Some(Self {
            handle,
            owned: false,
        })
    }

    /// Create a Channel from a typed handle without taking ownership.
    ///
    /// The handle will NOT be closed when this Channel is dropped.
    pub fn from_typed_borrowed(handle: ChannelHandle) -> Self {
        Self {
            handle,
            owned: false,
        }
    }

    /// Get the underlying typed handle.
    pub fn handle(&self) -> ChannelHandle {
        self.handle
    }

    /// Get the underlying handle as an untyped Handle.
    pub fn untyped_handle(&self) -> Handle {
        self.handle.into()
    }

    /// Send a message (blocking if queue is full).
    pub fn send(&self, msg: &[u8]) -> Result<()> {
        let result = sys::channel::send_msg(self.handle.into(), msg);
        if result < 0 {
            Err(error::from_code(result))
        } else {
            Ok(())
        }
    }

    /// Try to send a message (non-blocking).
    ///
    /// Returns `Err(ErrorCode::WouldBlock)` if the queue is full.
    pub fn try_send(&self, msg: &[u8]) -> Result<()> {
        let result = sys::channel::try_send_msg(self.handle.into(), msg);
        if result < 0 {
            Err(error::from_code(result))
        } else {
            Ok(())
        }
    }

    /// Receive a message (blocking if queue is empty).
    ///
    /// Returns the number of bytes received.
    pub fn recv(&self, buf: &mut [u8]) -> Result<usize> {
        let result = sys::channel::recv_msg(self.handle.into(), buf);
        if result < 0 {
            Err(error::from_code(result))
        } else {
            Ok(result as usize)
        }
    }

    /// Try to receive a message (non-blocking).
    ///
    /// Returns `Ok(Some(len))` if a message was received, `Ok(None)` if the
    /// queue is empty, or `Err` on error.
    pub fn try_recv(&self, buf: &mut [u8]) -> Result<Option<usize>> {
        let result = sys::channel::try_recv_msg(self.handle.into(), buf);
        if result == -1 {
            // Would block - no message available
            Ok(None)
        } else if result < 0 {
            Err(error::from_code(result))
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

    /// Consume the channel and return the underlying typed handle without closing it.
    pub fn into_handle(self) -> ChannelHandle {
        let handle = self.handle;
        core::mem::forget(self);
        handle
    }

    /// Consume the channel and return the underlying untyped handle without closing it.
    pub fn into_untyped_handle(self) -> Handle {
        self.into_handle().into()
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        if self.owned {
            let _ = sys::file::close(self.handle.into());
        }
    }
}

// =============================================================================
// Standalone channel functions (for use with raw handles)
// =============================================================================

/// Send a message on a channel (blocking if queue full).
#[inline(always)]
pub fn send(handle: Handle, msg: &[u8]) -> Result<()> {
    let result = sys::channel::send_msg(handle, msg);
    if result < 0 {
        Err(error::from_code(result))
    } else {
        Ok(())
    }
}

/// Send a message on a channel (non-blocking).
///
/// Returns `Err(ErrorCode::WouldBlock)` if the queue is full.
#[inline(always)]
pub fn try_send(handle: Handle, msg: &[u8]) -> Result<()> {
    let result = sys::channel::try_send_msg(handle, msg);
    if result < 0 {
        Err(error::from_code(result))
    } else {
        Ok(())
    }
}

/// Receive a message from a channel (blocking if queue empty).
///
/// Returns the number of bytes received on success.
#[inline(always)]
pub fn recv(handle: Handle, buf: &mut [u8]) -> Result<usize> {
    let result = sys::channel::recv_msg(handle, buf);
    if result < 0 {
        Err(error::from_code(result))
    } else {
        Ok(result as usize)
    }
}

/// Receive a message from a channel (non-blocking).
///
/// Returns `Ok(len)` on success, `Err(ErrorCode::WouldBlock)` if queue is empty.
#[inline(always)]
pub fn try_recv(handle: Handle, buf: &mut [u8]) -> Result<usize> {
    let result = sys::channel::try_recv_msg(handle, buf);
    if result < 0 {
        Err(error::from_code(result))
    } else {
        Ok(result as usize)
    }
}

/// Create a new channel pair.
///
/// Returns handles to both endpoints: `(endpoint_a, endpoint_b)`.
/// Messages sent on endpoint_a are received by endpoint_b, and vice versa.
pub fn create_pair() -> Result<(ChannelHandle, ChannelHandle)> {
    let mut handles: [u64; 2] = [0, 0];
    let result = sys::channel::create_raw(&mut handles);
    if result < 0 {
        return Err(error::from_code(result));
    }
    let handle_a = ChannelHandle::from_raw(handles[0]).ok_or(ErrorCode::InvalidArgument)?;
    let handle_b = ChannelHandle::from_raw(handles[1]).ok_or(ErrorCode::InvalidArgument)?;
    Ok((handle_a, handle_b))
}
