//! Userspace scheme provider support (M2.2).
//!
//! Register a scheme and serve `open`/`readdir`/`read`/`write`/`close`
//! requests routed to this process by the kernel. See docs/SYSCALLS.md
//! "Scheme provider operations" and `panda_abi::scheme_protocol` for the
//! wire format implemented here.
//!
//! # Example
//!
//! ```no_run
//! use libpanda::scheme::SchemeProvider;
//! use panda_abi::scheme_protocol::Request;
//! use panda_abi::MAX_MESSAGE_SIZE;
//!
//! let provider = SchemeProvider::register("echo").unwrap();
//! let mut buf = [0u8; MAX_MESSAGE_SIZE];
//! loop {
//!     let request = match provider.recv(&mut buf) {
//!         Ok(r) => r,
//!         Err(_) => break, // kernel side closed
//!     };
//!     match request {
//!         Request::Open { request_id, .. } => {
//!             let _ = provider.reply_open_ok(request_id, 1);
//!         }
//!         _ => {}
//!     }
//! }
//! ```

use crate::error::{self, Result};
use crate::ipc::Channel;
use crate::sys;
use panda_abi::ErrorCode;
use panda_abi::scheme_protocol::{self, ReaddirEntry, Request, Response};

/// A registered scheme provider.
///
/// Owns the channel endpoint the kernel uses to route `open`/`readdir`/
/// `read`/`write`/`close` requests to this process. Requests arrive via
/// [`SchemeProvider::recv`]; each has a `request_id` that MUST be echoed
/// back verbatim in the matching `reply_*` call (see the module docs on
/// `panda_abi::scheme_protocol` for why: it's what lets a fire-and-forget
/// `Close` ack sent after this process has moved on to a later request be
/// told apart from that later request's real response, on the kernel
/// side).
pub struct SchemeProvider {
    channel: Channel,
}

impl SchemeProvider {
    /// Register `name` as a scheme this process will serve.
    ///
    /// Fails with `AlreadyExists` if the name is already registered by
    /// another provider, or `InvalidArgument` for an empty name.
    pub fn register(name: &str) -> Result<Self> {
        let handle = error::from_syscall_handle(sys::scheme::register(name))?;
        let channel = Channel::from_handle(handle).ok_or(ErrorCode::InvalidHandle)?;
        Ok(Self { channel })
    }

    /// Receive and decode the next request (blocking).
    ///
    /// Returns `Err` if the kernel's endpoint has closed — this shouldn't
    /// normally happen while the process is alive, so callers typically
    /// treat it as "stop serving".
    pub fn recv<'b>(&self, buf: &'b mut [u8; panda_abi::MAX_MESSAGE_SIZE]) -> Result<Request<'b>> {
        let len = self.channel.recv(buf)?;
        Request::decode(&buf[..len]).ok_or(ErrorCode::Protocol)
    }

    /// Receive and decode the next request without blocking.
    ///
    /// Returns `Ok(None)` if no request is currently queued — useful for a
    /// provider that also needs to watch another channel (e.g. its own
    /// parent channel) in the same loop, since this codebase has no
    /// single-process multi-channel blocking recv outside of `Mailbox`.
    pub fn try_recv<'b>(
        &self,
        buf: &'b mut [u8; panda_abi::MAX_MESSAGE_SIZE],
    ) -> Result<Option<Request<'b>>> {
        match self.channel.try_recv(buf)? {
            Some(len) => Ok(Some(
                Request::decode(&buf[..len]).ok_or(ErrorCode::Protocol)?,
            )),
            None => Ok(None),
        }
    }

    fn send_encoded(&self, encode: impl FnOnce(&mut [u8]) -> Option<usize>) -> Result<()> {
        let mut buf = [0u8; panda_abi::MAX_MESSAGE_SIZE];
        let len = encode(&mut buf).ok_or(ErrorCode::MessageTooLarge)?;
        self.channel.send(&buf[..len])
    }

    /// Reply to an `Open` request with the resource id this provider minted
    /// for it (scoped to this provider — the kernel treats it as opaque).
    pub fn reply_open_ok(&self, request_id: u64, resource_id: u64) -> Result<()> {
        self.send_encoded(|buf| Response::encode_open_ok(request_id, resource_id, buf))
    }

    /// Reply to an `Open` request with an error (e.g. `NotFound` for an
    /// unrecognized path).
    pub fn reply_open_err(&self, request_id: u64, error: ErrorCode) -> Result<()> {
        self.send_encoded(|buf| {
            Response::encode_err(scheme_protocol::MSG_OPEN, request_id, error, buf)
        })
    }

    /// Reply to a `Readdir` request with a directory listing.
    ///
    /// Must fit in one `MAX_MESSAGE_SIZE` frame — see
    /// `panda_abi::scheme_protocol`'s module docs on why large directories
    /// are out of scope for v1 (returns `MessageTooLarge` if it doesn't fit
    /// rather than paginating).
    pub fn reply_readdir_ok(&self, request_id: u64, entries: &[ReaddirEntry<'_>]) -> Result<()> {
        self.send_encoded(|buf| Response::encode_readdir_ok(request_id, entries, buf))
    }

    /// Reply to a `Readdir` request with an error.
    pub fn reply_readdir_err(&self, request_id: u64, error: ErrorCode) -> Result<()> {
        self.send_encoded(|buf| {
            Response::encode_err(scheme_protocol::MSG_READDIR, request_id, error, buf)
        })
    }

    /// Reply to a `Read` request with up to the requested number of bytes.
    pub fn reply_read_ok(&self, request_id: u64, data: &[u8]) -> Result<()> {
        self.send_encoded(|buf| Response::encode_read_ok(request_id, data, buf))
    }

    /// Reply to a `Read` request with an error.
    pub fn reply_read_err(&self, request_id: u64, error: ErrorCode) -> Result<()> {
        self.send_encoded(|buf| {
            Response::encode_err(scheme_protocol::MSG_READ, request_id, error, buf)
        })
    }

    /// Reply to a `Write` request with the number of bytes accepted.
    pub fn reply_write_ok(&self, request_id: u64, written: u32) -> Result<()> {
        self.send_encoded(|buf| Response::encode_write_ok(request_id, written, buf))
    }

    /// Reply to a `Write` request with an error.
    pub fn reply_write_err(&self, request_id: u64, error: ErrorCode) -> Result<()> {
        self.send_encoded(|buf| {
            Response::encode_err(scheme_protocol::MSG_WRITE, request_id, error, buf)
        })
    }

    /// Acknowledge a `Close` request.
    ///
    /// Note the kernel also sends `Close` as a fire-and-forget notification
    /// when a client's handle is dropped (explicit close or process exit) —
    /// it does not wait for this ack, so a provider that just drops its
    /// bookkeeping for `resource_id` without replying is still correct.
    /// Replying is nonetheless recommended so `reply_*` failures on this
    /// provider's side surface the same way as any other request.
    pub fn reply_close_ok(&self, request_id: u64) -> Result<()> {
        self.send_encoded(|buf| Response::encode_close_ok(request_id, buf))
    }

    /// Reply to a `Close` request with an error.
    pub fn reply_close_err(&self, request_id: u64, error: ErrorCode) -> Result<()> {
        self.send_encoded(|buf| {
            Response::encode_err(scheme_protocol::MSG_CLOSE, request_id, error, buf)
        })
    }

    /// Reply to a `Connect` request by handing the caller a live channel.
    ///
    /// `channel` is attached to the response frame using the same
    /// handle-transfer mechanism `Channel::send_with_handle` uses elsewhere
    /// (see docs/IPC.md "Handle transfer"): the kernel installs it directly
    /// into the connecting process's handle table, bypassing the
    /// resource_id proxy `reply_open_ok` sets up. Typically `channel` is the
    /// caller-facing half of a fresh [`crate::ipc::create_pair`], with the
    /// other half kept by this provider as the new connection.
    pub fn reply_connect_ok(&self, request_id: u64, channel: &Channel) -> Result<()> {
        let mut buf = [0u8; panda_abi::MAX_MESSAGE_SIZE];
        let len = Response::encode_connect_ok(request_id, &mut buf).ok_or(ErrorCode::MessageTooLarge)?;
        self.channel.send_with_handle(&buf[..len], channel.untyped_handle())
    }

    /// Reply to a `Connect` request with an error (e.g. `NotFound` for an
    /// unrecognized path).
    pub fn reply_connect_err(&self, request_id: u64, error: ErrorCode) -> Result<()> {
        self.send_encoded(|buf| {
            Response::encode_err(scheme_protocol::MSG_CONNECT, request_id, error, buf)
        })
    }

    /// Reply to a `Size` request with the resource's byte size.
    pub fn reply_size_ok(&self, request_id: u64, size: u64) -> Result<()> {
        self.send_encoded(|buf| Response::encode_size_ok(request_id, size, buf))
    }

    /// Reply to a `Size` request with an error (e.g. `NotSupported` for a
    /// stream-like resource with no addressable size).
    pub fn reply_size_err(&self, request_id: u64, error: ErrorCode) -> Result<()> {
        self.send_encoded(|buf| {
            Response::encode_err(scheme_protocol::MSG_SIZE, request_id, error, buf)
        })
    }

    /// Reject `request` with `NotSupported`, whatever kind it is.
    ///
    /// Every provider serves a subset of the protocol and has to answer the
    /// rest *somehow* — a request left unanswered leaves the caller blocked
    /// in `round_trip` forever. Without this, each provider needed one
    /// hand-written rejection arm per unsupported kind, which is how
    /// `Request` gaining the M5 filesystem kinds broke the build of all
    /// three drivers and the compositor at once. The kind-to-reply mapping
    /// belongs here, next to the reply helpers, rather than being repeated
    /// in every serve loop.
    pub fn reply_unsupported(&self, request: &Request<'_>) -> Result<()> {
        let request_id = request.request_id();
        let error = ErrorCode::NotSupported;
        match request {
            Request::Open { .. } => self.reply_open_err(request_id, error),
            Request::Readdir { .. } => self.reply_readdir_err(request_id, error),
            Request::Read { .. } => self.reply_read_err(request_id, error),
            Request::Write { .. } => self.reply_write_err(request_id, error),
            Request::Close { .. } => self.reply_close_err(request_id, error),
            Request::Connect { .. } => self.reply_connect_err(request_id, error),
            Request::Size { .. } => self.reply_size_err(request_id, error),
            Request::Stat { .. } => self.reply_stat_err(request_id, error),
            Request::Create { .. } => self.reply_create_err(request_id, error),
            Request::Unlink { .. } => {
                self.reply_path_op_err(scheme_protocol::MSG_UNLINK, request_id, error)
            }
            Request::Mkdir { .. } => {
                self.reply_path_op_err(scheme_protocol::MSG_MKDIR, request_id, error)
            }
            Request::Rmdir { .. } => {
                self.reply_path_op_err(scheme_protocol::MSG_RMDIR, request_id, error)
            }
            Request::DisplayInfo { .. } => self.reply_display_info_err(request_id, error),
            Request::DisplayMap { .. } => self.reply_display_map_err(request_id, error),
            Request::DisplayFlush { .. } => self.reply_display_flush_err(request_id, error),
        }
    }

    /// Reply to a `Stat` request with the resource's size and whether it is
    /// a directory — see `panda_abi::scheme_protocol::MSG_STAT`.
    pub fn reply_stat_ok(&self, request_id: u64, size: u64, is_dir: bool) -> Result<()> {
        self.send_encoded(|buf| Response::encode_stat_ok(request_id, size, is_dir, buf))
    }

    /// Reply to a `Stat` request with an error.
    pub fn reply_stat_err(&self, request_id: u64, error: ErrorCode) -> Result<()> {
        self.send_encoded(|buf| {
            Response::encode_err(scheme_protocol::MSG_STAT, request_id, error, buf)
        })
    }

    /// Reply to a `Create` request with the newly created file's resource
    /// id (the same contract as [`reply_open_ok`](Self::reply_open_ok): the
    /// file comes back open) — see
    /// `panda_abi::scheme_protocol::MSG_CREATE`.
    pub fn reply_create_ok(&self, request_id: u64, resource_id: u64) -> Result<()> {
        self.send_encoded(|buf| Response::encode_create_ok(request_id, resource_id, buf))
    }

    /// Reply to a `Create` request with an error.
    pub fn reply_create_err(&self, request_id: u64, error: ErrorCode) -> Result<()> {
        self.send_encoded(|buf| {
            Response::encode_err(scheme_protocol::MSG_CREATE, request_id, error, buf)
        })
    }

    /// Acknowledge an `Unlink`/`Mkdir`/`Rmdir` request. `kind` must be the
    /// `MSG_*` constant of the request being answered, since those three
    /// share one payload-free response shape.
    pub fn reply_path_op_ok(&self, kind: u8, request_id: u64) -> Result<()> {
        self.send_encoded(|buf| Response::encode_path_op_ok(kind, request_id, buf))
    }

    /// Reply to an `Unlink`/`Mkdir`/`Rmdir` request with an error. See
    /// [`reply_path_op_ok`](Self::reply_path_op_ok) for `kind`.
    pub fn reply_path_op_err(&self, kind: u8, request_id: u64, error: ErrorCode) -> Result<()> {
        self.send_encoded(|buf| Response::encode_err(kind, request_id, error, buf))
    }

    /// Reply to a `DisplayInfo` request with the display's mode info.
    pub fn reply_display_info_ok(
        &self,
        request_id: u64,
        width: u32,
        height: u32,
        format: u32,
        stride: u32,
    ) -> Result<()> {
        self.send_encoded(|buf| {
            Response::encode_display_info_ok(request_id, width, height, format, stride, buf)
        })
    }

    /// Reply to a `DisplayInfo` request with an error.
    pub fn reply_display_info_err(&self, request_id: u64, error: ErrorCode) -> Result<()> {
        self.send_encoded(|buf| {
            Response::encode_err(scheme_protocol::MSG_DISPLAY_INFO, request_id, error, buf)
        })
    }

    /// Reply to a `DisplayMap` request with the framebuffer's guest-physical
    /// address and byte size — see [`panda_abi::scheme_protocol::MSG_DISPLAY_MAP`]'s
    /// doc comment for why this is a raw physical range rather than opaque
    /// bytes.
    pub fn reply_display_map_ok(&self, request_id: u64, phys_addr: u64, size: u64) -> Result<()> {
        self.send_encoded(|buf| Response::encode_display_map_ok(request_id, phys_addr, size, buf))
    }

    /// Reply to a `DisplayMap` request with an error.
    pub fn reply_display_map_err(&self, request_id: u64, error: ErrorCode) -> Result<()> {
        self.send_encoded(|buf| {
            Response::encode_err(scheme_protocol::MSG_DISPLAY_MAP, request_id, error, buf)
        })
    }

    /// Acknowledge a `DisplayFlush` request.
    pub fn reply_display_flush_ok(&self, request_id: u64) -> Result<()> {
        self.send_encoded(|buf| Response::encode_display_flush_ok(request_id, buf))
    }

    /// Reply to a `DisplayFlush` request with an error.
    pub fn reply_display_flush_err(&self, request_id: u64, error: ErrorCode) -> Result<()> {
        self.send_encoded(|buf| {
            Response::encode_err(scheme_protocol::MSG_DISPLAY_FLUSH, request_id, error, buf)
        })
    }

    /// Push an unsolicited notification for `resource_id`, waking any
    /// mailbox a client attached to it (via `OP_ENVIRONMENT_OPEN`'s
    /// `mailbox`/`event_mask` arguments) with `events`. Not a reply to any
    /// request — see `panda_abi::scheme_protocol::MSG_NOTIFY`.
    pub fn notify(&self, resource_id: u64, events: u32) -> Result<()> {
        self.send_encoded(|buf| Response::encode_notify(resource_id, events, buf))
    }
}
