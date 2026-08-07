//! Wire protocol for userspace scheme providers (M2.2).
//!
//! A scheme provider serves `open`/`readdir`/`read`/`write`/`close` requests
//! from the kernel over an ordinary channel (see `OP_SCHEME_REGISTER` in
//! `docs/SYSCALLS.md` and the provider protocol section of `docs/IPC.md`).
//! Requests and responses are hand-rolled binary frames, in the same spirit
//! as the `StartupMessageHeader` format documented in `docs/IPC.md`: a small
//! fixed header followed by variable trailing bytes. Every frame fits in one
//! `MAX_MESSAGE_SIZE` channel message — there is no fragmentation.
//!
//! This module has no allocator dependency (`panda-abi` only has `alloc`
//! under the `std` feature) — all encode/decode functions operate on
//! caller-provided byte slices.
//!
//! # Request frame
//!
//! ```text
//! byte 0:      kind (u8, one of MSG_*)
//! bytes 1..9:  request_id (u64 LE) — minted by the kernel, echoed verbatim
//!              in the response so the (currently trivial, single-in-flight)
//!              correlation is future-proofed for real multiplexing.
//! bytes 9..:   kind-specific payload, see `Request`
//! ```
//!
//! # Response frame
//!
//! ```text
//! byte 0:      kind (u8, same MSG_* as the request it answers)
//! bytes 1..9:  request_id (u64 LE), echoed from the request
//! byte 9:      status (u8, STATUS_OK or STATUS_ERR)
//! bytes 10..:  status- and kind-specific payload, see `Response`
//! ```

use crate::ErrorCode;

/// Request/response kind: open a resource at a path.
pub const MSG_OPEN: u8 = 1;
/// Request/response kind: list directory entries at a path.
pub const MSG_READDIR: u8 = 2;
/// Request/response kind: read bytes from a provider resource.
pub const MSG_READ: u8 = 3;
/// Request/response kind: write bytes to a provider resource.
pub const MSG_WRITE: u8 = 4;
/// Request/response kind: close a provider resource.
pub const MSG_CLOSE: u8 = 5;
/// Request/response kind: connect to the provider and receive a live
/// channel, rather than a file-like `resource_id` (see [`Request::Connect`]
/// and the "Connect" section of the module docs above the request enum).
pub const MSG_CONNECT: u8 = 6;
/// Response-direction-only kind: an unsolicited push from the provider to
/// the kernel telling it a resource has new events ready (e.g. buffered
/// keyboard input), so the kernel can wake any mailbox a client attached to
/// that resource with `OP_ENVIRONMENT_OPEN`'s `mailbox`/`event_mask`
/// arguments. Not a reply to any request — always carries `request_id ==
/// 0` (see [`Response::encode_notify`]/[`ProviderState`]'s background
/// reader in `panda-kernel/src/resource/scheme.rs`, which routes frames
/// with `request_id == 0` to attached mailboxes instead of a pending
/// round-trip). Added in Phase 7 for the userspace virtio-keyboard driver;
/// see `plans/device-driver-model.md`'s Phase 7 landed-deviations note.
pub const MSG_NOTIFY: u8 = 7;
/// Request/response kind: query a provider resource's byte size. Added for
/// M4's block driver service (`resource::block_client::SchemeBlockDevice`):
/// the kernel's `BlockDevice` trait needs a synchronous `size()` accessor,
/// so the size is fetched once (via this request) and cached at open time
/// rather than being queryable per-call. Not meaningful for stream-like
/// resources (e.g. `keyboard:`'s eventq) — those providers should reply
/// `SizeErr(NotSupported)`.
pub const MSG_SIZE: u8 = 8;
/// Request/response kind: fetch a display resource's mode info (width,
/// height, pixel format, stride). Added in roadmap M4 so `OP_DISPLAY_INFO`
/// can be served by a userspace display driver (via `SchemeProxyResource`)
/// instead of only a kernel-resident display resource — see
/// `panda-kernel/src/syscall/display.rs`'s scheme-proxy forwarding branch
/// and `plans/device-driver-model.md`'s M4 landed-deviations note.
pub const MSG_DISPLAY_INFO: u8 = 9;
/// Request/response kind: map a display resource's framebuffer into the
/// calling process. Unlike every other request kind, `DisplayMapOk`'s
/// payload carries a raw guest-physical address + size rather than opaque
/// bytes — the kernel maps that physical range directly into the caller,
/// the same way the old kernel-resident display driver did, instead of
/// proxying page contents through read/write. This means the provider is
/// trusted to disclose a real, exclusively-owned physical range: only the
/// blessed display driver should ever hold the `display:` scheme
/// registration in practice; see the M4 landed-deviations note for the
/// follow-up scheme-registration ACL gap this leaves (any process can
/// currently call `OP_SCHEME_REGISTER` for any name).
pub const MSG_DISPLAY_MAP: u8 = 10;
/// Request/response kind: flush a damaged rectangle (or, if none given, the
/// whole surface) to the display.
pub const MSG_DISPLAY_FLUSH: u8 = 11;

/// Response status byte: the operation succeeded.
pub const STATUS_OK: u8 = 0;
/// Response status byte: the operation failed; an `ErrorCode` byte follows.
pub const STATUS_ERR: u8 = 1;

const HEADER_LEN: usize = 9; // kind(1) + request_id(8)
const RESP_HEADER_LEN: usize = HEADER_LEN + 1; // + status(1)

/// Largest `read`/`write` payload guaranteed to fit in one
/// `MAX_MESSAGE_SIZE` frame in either direction, leaving headroom for the
/// largest header (a `Write` request: kind + request_id + resource_id +
/// data_len = 21 bytes). Callers (kernel syscall handlers, provider serve
/// loops) should cap transfer sizes to this.
pub const MAX_TRANSFER_SIZE: usize = crate::MAX_MESSAGE_SIZE - 96;

fn error_to_u8(err: ErrorCode) -> u8 {
    err as u32 as u8
}

fn u8_to_error(byte: u8) -> ErrorCode {
    match byte {
        1 => ErrorCode::NotFound,
        2 => ErrorCode::InvalidOffset,
        3 => ErrorCode::NotReadable,
        4 => ErrorCode::NotWritable,
        5 => ErrorCode::NotSeekable,
        6 => ErrorCode::NotSupported,
        7 => ErrorCode::PermissionDenied,
        9 => ErrorCode::WouldBlock,
        10 => ErrorCode::InvalidArgument,
        11 => ErrorCode::Protocol,
        12 => ErrorCode::InvalidHandle,
        13 => ErrorCode::TooManyHandles,
        14 => ErrorCode::ChannelClosed,
        15 => ErrorCode::MessageTooLarge,
        16 => ErrorCode::BufferTooSmall,
        17 => ErrorCode::AlreadyExists,
        18 => ErrorCode::NoSpace,
        19 => ErrorCode::NotEmpty,
        20 => ErrorCode::IsDirectory,
        21 => ErrorCode::NotDirectory,
        22 => ErrorCode::Busy,
        // 8 (IoError) and anything unrecognized collapse to IoError: a
        // provider is untrusted input, so a malformed/unknown error byte
        // must not be treated as success.
        _ => ErrorCode::IoError,
    }
}

/// A decoded provider request (borrows from the frame it was decoded from).
///
/// # Connect
///
/// `Connect` is the odd one out: `Open`/`Readdir`/`Read`/`Write`/`Close` are
/// all file-like — a resource opened this way is addressed by an opaque
/// `resource_id` the provider mints, and every operation after `Open` is a
/// synchronous round trip relayed by the kernel. `Connect` instead asks the
/// provider to hand back a **live channel**: the provider replies with
/// [`Response::ConnectOk`] over its own provider channel, using the
/// underlying channel-attachment mechanism (see docs/IPC.md "Handle
/// transfer") to carry a fresh `ChannelEndpoint`, which the kernel installs
/// directly into the calling process's handle table (bypassing the
/// resource_id proxy entirely — see
/// `resource::scheme::UserSchemeProvider::connect` and
/// `OP_ENVIRONMENT_CONNECT`). This is how a process gets a full-duplex,
/// unmediated channel to another process's own custom protocol (e.g. the
/// compositor's `Request`/`Event` frames) by name, instead of relying on
/// being spawned by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request<'a> {
    Open { request_id: u64, path: &'a str },
    Readdir { request_id: u64, path: &'a str },
    /// `offset` is a byte offset into the resource, meaningful for
    /// random-access resources (e.g. block devices, via
    /// `resource::block_client::SchemeBlockDevice`). Stream-like providers
    /// (e.g. `keyboard:`'s eventq) may ignore it — they have no addressable
    /// position, only a FIFO of buffered data.
    Read { request_id: u64, resource_id: u64, offset: u64, len: u32, nonblocking: bool },
    /// `offset` — see `Request::Read`'s doc comment.
    Write { request_id: u64, resource_id: u64, offset: u64, data: &'a [u8] },
    Close { request_id: u64, resource_id: u64 },
    Connect { request_id: u64, path: &'a str },
    /// Query a resource's byte size — see [`MSG_SIZE`].
    Size { request_id: u64, resource_id: u64 },
    /// See [`MSG_DISPLAY_INFO`].
    DisplayInfo { request_id: u64, resource_id: u64 },
    /// See [`MSG_DISPLAY_MAP`].
    DisplayMap { request_id: u64, resource_id: u64 },
    /// See [`MSG_DISPLAY_FLUSH`]. `rect` is `None` for a full-surface flush.
    DisplayFlush {
        request_id: u64,
        resource_id: u64,
        rect: Option<(u32, u32, u32, u32)>,
    },
}

impl<'a> Request<'a> {
    /// The request_id carried by any request variant.
    pub fn request_id(self) -> u64 {
        match self {
            Request::Open { request_id, .. }
            | Request::Readdir { request_id, .. }
            | Request::Read { request_id, .. }
            | Request::Write { request_id, .. }
            | Request::Close { request_id, .. }
            | Request::Connect { request_id, .. }
            | Request::Size { request_id, .. }
            | Request::DisplayInfo { request_id, .. }
            | Request::DisplayMap { request_id, .. }
            | Request::DisplayFlush { request_id, .. } => request_id,
        }
    }

    /// Encode into `buf`. Returns the encoded length, or `None` if it
    /// doesn't fit.
    pub fn encode(self, buf: &mut [u8]) -> Option<usize> {
        match self {
            Request::Open { request_id, path }
            | Request::Readdir { request_id, path }
            | Request::Connect { request_id, path } => {
                let kind = if matches!(self, Request::Open { .. }) {
                    MSG_OPEN
                } else if matches!(self, Request::Readdir { .. }) {
                    MSG_READDIR
                } else {
                    MSG_CONNECT
                };
                let path = path.as_bytes();
                let total = HEADER_LEN + 2 + path.len();
                if total > buf.len() || path.len() > u16::MAX as usize {
                    return None;
                }
                buf[0] = kind;
                buf[1..9].copy_from_slice(&request_id.to_le_bytes());
                buf[9..11].copy_from_slice(&(path.len() as u16).to_le_bytes());
                buf[11..11 + path.len()].copy_from_slice(path);
                Some(total)
            }
            Request::Read {
                request_id,
                resource_id,
                offset,
                len,
                nonblocking,
            } => {
                let total = HEADER_LEN + 8 + 8 + 4 + 1;
                if total > buf.len() {
                    return None;
                }
                buf[0] = MSG_READ;
                buf[1..9].copy_from_slice(&request_id.to_le_bytes());
                buf[9..17].copy_from_slice(&resource_id.to_le_bytes());
                buf[17..25].copy_from_slice(&offset.to_le_bytes());
                buf[25..29].copy_from_slice(&len.to_le_bytes());
                buf[29] = nonblocking as u8;
                Some(total)
            }
            Request::Write {
                request_id,
                resource_id,
                offset,
                data,
            } => {
                let total = HEADER_LEN + 8 + 8 + 4 + data.len();
                if total > buf.len() {
                    return None;
                }
                buf[0] = MSG_WRITE;
                buf[1..9].copy_from_slice(&request_id.to_le_bytes());
                buf[9..17].copy_from_slice(&resource_id.to_le_bytes());
                buf[17..25].copy_from_slice(&offset.to_le_bytes());
                buf[25..29].copy_from_slice(&(data.len() as u32).to_le_bytes());
                buf[29..29 + data.len()].copy_from_slice(data);
                Some(total)
            }
            Request::Close {
                request_id,
                resource_id,
            } => {
                let total = HEADER_LEN + 8;
                if total > buf.len() {
                    return None;
                }
                buf[0] = MSG_CLOSE;
                buf[1..9].copy_from_slice(&request_id.to_le_bytes());
                buf[9..17].copy_from_slice(&resource_id.to_le_bytes());
                Some(total)
            }
            Request::Size {
                request_id,
                resource_id,
            } => {
                let total = HEADER_LEN + 8;
                if total > buf.len() {
                    return None;
                }
                buf[0] = MSG_SIZE;
                buf[1..9].copy_from_slice(&request_id.to_le_bytes());
                buf[9..17].copy_from_slice(&resource_id.to_le_bytes());
                Some(total)
            }
            Request::DisplayInfo { request_id, resource_id } => {
                let total = HEADER_LEN + 8;
                if total > buf.len() {
                    return None;
                }
                buf[0] = MSG_DISPLAY_INFO;
                buf[1..9].copy_from_slice(&request_id.to_le_bytes());
                buf[9..17].copy_from_slice(&resource_id.to_le_bytes());
                Some(total)
            }
            Request::DisplayMap { request_id, resource_id } => {
                let total = HEADER_LEN + 8;
                if total > buf.len() {
                    return None;
                }
                buf[0] = MSG_DISPLAY_MAP;
                buf[1..9].copy_from_slice(&request_id.to_le_bytes());
                buf[9..17].copy_from_slice(&resource_id.to_le_bytes());
                Some(total)
            }
            Request::DisplayFlush { request_id, resource_id, rect } => {
                let total = HEADER_LEN + 8 + 1 + 16;
                if total > buf.len() {
                    return None;
                }
                buf[0] = MSG_DISPLAY_FLUSH;
                buf[1..9].copy_from_slice(&request_id.to_le_bytes());
                buf[9..17].copy_from_slice(&resource_id.to_le_bytes());
                match rect {
                    Some((x, y, width, height)) => {
                        buf[17] = 1;
                        buf[18..22].copy_from_slice(&x.to_le_bytes());
                        buf[22..26].copy_from_slice(&y.to_le_bytes());
                        buf[26..30].copy_from_slice(&width.to_le_bytes());
                        buf[30..34].copy_from_slice(&height.to_le_bytes());
                    }
                    None => {
                        buf[17] = 0;
                        buf[18..34].fill(0);
                    }
                }
                Some(total)
            }
        }
    }

    /// Decode a request frame from `buf`. Returns `None` on truncated or
    /// malformed input.
    pub fn decode(buf: &'a [u8]) -> Option<Request<'a>> {
        if buf.len() < HEADER_LEN {
            return None;
        }
        let kind = buf[0];
        let request_id = u64::from_le_bytes(buf[1..9].try_into().ok()?);
        match kind {
            MSG_OPEN | MSG_READDIR | MSG_CONNECT => {
                if buf.len() < 11 {
                    return None;
                }
                let path_len = u16::from_le_bytes(buf[9..11].try_into().ok()?) as usize;
                let end = 11usize.checked_add(path_len)?;
                let path = core::str::from_utf8(buf.get(11..end)?).ok()?;
                Some(if kind == MSG_OPEN {
                    Request::Open { request_id, path }
                } else if kind == MSG_READDIR {
                    Request::Readdir { request_id, path }
                } else {
                    Request::Connect { request_id, path }
                })
            }
            MSG_READ => {
                if buf.len() < 29 {
                    return None;
                }
                let resource_id = u64::from_le_bytes(buf[9..17].try_into().ok()?);
                let offset = u64::from_le_bytes(buf[17..25].try_into().ok()?);
                let len = u32::from_le_bytes(buf[25..29].try_into().ok()?);
                let nonblocking = buf.get(29).copied().unwrap_or(0) != 0;
                Some(Request::Read {
                    request_id,
                    resource_id,
                    offset,
                    len,
                    nonblocking,
                })
            }
            MSG_WRITE => {
                if buf.len() < 29 {
                    return None;
                }
                let resource_id = u64::from_le_bytes(buf[9..17].try_into().ok()?);
                let offset = u64::from_le_bytes(buf[17..25].try_into().ok()?);
                let data_len = u32::from_le_bytes(buf[25..29].try_into().ok()?) as usize;
                let end = 29usize.checked_add(data_len)?;
                let data = buf.get(29..end)?;
                Some(Request::Write {
                    request_id,
                    resource_id,
                    offset,
                    data,
                })
            }
            MSG_CLOSE => {
                if buf.len() < 17 {
                    return None;
                }
                let resource_id = u64::from_le_bytes(buf[9..17].try_into().ok()?);
                Some(Request::Close {
                    request_id,
                    resource_id,
                })
            }
            MSG_SIZE => {
                if buf.len() < 17 {
                    return None;
                }
                let resource_id = u64::from_le_bytes(buf[9..17].try_into().ok()?);
                Some(Request::Size {
                    request_id,
                    resource_id,
                })
            }
            MSG_DISPLAY_INFO => {
                if buf.len() < 17 {
                    return None;
                }
                let resource_id = u64::from_le_bytes(buf[9..17].try_into().ok()?);
                Some(Request::DisplayInfo { request_id, resource_id })
            }
            MSG_DISPLAY_MAP => {
                if buf.len() < 17 {
                    return None;
                }
                let resource_id = u64::from_le_bytes(buf[9..17].try_into().ok()?);
                Some(Request::DisplayMap { request_id, resource_id })
            }
            MSG_DISPLAY_FLUSH => {
                if buf.len() < 34 {
                    return None;
                }
                let resource_id = u64::from_le_bytes(buf[9..17].try_into().ok()?);
                let rect = if buf[17] != 0 {
                    Some((
                        u32::from_le_bytes(buf[18..22].try_into().ok()?),
                        u32::from_le_bytes(buf[22..26].try_into().ok()?),
                        u32::from_le_bytes(buf[26..30].try_into().ok()?),
                        u32::from_le_bytes(buf[30..34].try_into().ok()?),
                    ))
                } else {
                    None
                };
                Some(Request::DisplayFlush { request_id, resource_id, rect })
            }
            _ => None,
        }
    }
}

/// One directory entry as carried in a `MSG_READDIR` response frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaddirEntry<'a> {
    pub name: &'a str,
    pub is_dir: bool,
}

/// A decoded provider response (borrows from the frame it was decoded from).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response<'a> {
    OpenOk { request_id: u64, resource_id: u64 },
    OpenErr { request_id: u64, error: ErrorCode },
    /// Readdir entries are not eagerly collected — call
    /// [`ReaddirEntriesIter::new`] on `raw` to iterate them without an
    /// allocator.
    ReaddirOk { request_id: u64, raw: &'a [u8] },
    ReaddirErr { request_id: u64, error: ErrorCode },
    ReadOk { request_id: u64, data: &'a [u8] },
    ReadErr { request_id: u64, error: ErrorCode },
    WriteOk { request_id: u64, written: u32 },
    WriteErr { request_id: u64, error: ErrorCode },
    CloseOk { request_id: u64 },
    CloseErr { request_id: u64, error: ErrorCode },
    /// Answers `Request::Connect`. The live channel itself travels as an
    /// attached handle on the frame carrying this response (see
    /// `Request::Connect`'s doc comment) — there is no payload to decode
    /// here beyond the header.
    ConnectOk { request_id: u64 },
    ConnectErr { request_id: u64, error: ErrorCode },
    /// Answers `Request::Size` — see [`MSG_SIZE`].
    SizeOk { request_id: u64, size: u64 },
    SizeErr { request_id: u64, error: ErrorCode },
    /// Unsolicited push — see [`MSG_NOTIFY`]. `request_id` is always `0`.
    Notify { resource_id: u64, events: u32 },
    /// Answers [`Request::DisplayInfo`]. `format` mirrors
    /// `panda_abi::SurfaceInfoOut::format` (currently always ARGB8888 = 0);
    /// `stride` is bytes per row.
    DisplayInfoOk { request_id: u64, width: u32, height: u32, format: u32, stride: u32 },
    DisplayInfoErr { request_id: u64, error: ErrorCode },
    /// Answers [`Request::DisplayMap`] — see [`MSG_DISPLAY_MAP`]'s doc
    /// comment for why this carries a raw physical address instead of an
    /// opaque handle/bytes.
    DisplayMapOk { request_id: u64, phys_addr: u64, size: u64 },
    DisplayMapErr { request_id: u64, error: ErrorCode },
    DisplayFlushOk { request_id: u64 },
    DisplayFlushErr { request_id: u64, error: ErrorCode },
}

impl<'a> Response<'a> {
    pub fn request_id(self) -> u64 {
        match self {
            Response::OpenOk { request_id, .. }
            | Response::OpenErr { request_id, .. }
            | Response::ReaddirOk { request_id, .. }
            | Response::ReaddirErr { request_id, .. }
            | Response::ReadOk { request_id, .. }
            | Response::ReadErr { request_id, .. }
            | Response::WriteOk { request_id, .. }
            | Response::WriteErr { request_id, .. }
            | Response::CloseOk { request_id }
            | Response::CloseErr { request_id, .. }
            | Response::ConnectOk { request_id }
            | Response::ConnectErr { request_id, .. }
            | Response::SizeOk { request_id, .. }
            | Response::SizeErr { request_id, .. }
            | Response::DisplayInfoOk { request_id, .. }
            | Response::DisplayInfoErr { request_id, .. }
            | Response::DisplayMapOk { request_id, .. }
            | Response::DisplayMapErr { request_id, .. }
            | Response::DisplayFlushOk { request_id }
            | Response::DisplayFlushErr { request_id, .. } => request_id,
            Response::Notify { .. } => 0,
        }
    }

    fn header(kind: u8, request_id: u64, status: u8, buf: &mut [u8]) -> Option<()> {
        if buf.len() < RESP_HEADER_LEN {
            return None;
        }
        buf[0] = kind;
        buf[1..9].copy_from_slice(&request_id.to_le_bytes());
        buf[9] = status;
        Some(())
    }

    /// Encode `OpenOk`.
    pub fn encode_open_ok(request_id: u64, resource_id: u64, buf: &mut [u8]) -> Option<usize> {
        let total = RESP_HEADER_LEN + 8;
        if total > buf.len() {
            return None;
        }
        Self::header(MSG_OPEN, request_id, STATUS_OK, buf)?;
        buf[10..18].copy_from_slice(&resource_id.to_le_bytes());
        Some(total)
    }

    /// Encode a simple `<Kind>Err` frame (every error variant shares this shape).
    pub fn encode_err(kind: u8, request_id: u64, error: ErrorCode, buf: &mut [u8]) -> Option<usize> {
        let total = RESP_HEADER_LEN + 1;
        if total > buf.len() {
            return None;
        }
        Self::header(kind, request_id, STATUS_ERR, buf)?;
        buf[10] = error_to_u8(error);
        Some(total)
    }

    /// Encode `ReaddirOk` from a slice of entries. Returns `None` if the
    /// entries don't fit in one frame (see module docs — pagination is a
    /// documented follow-up, not solved here).
    pub fn encode_readdir_ok(
        request_id: u64,
        entries: &[ReaddirEntry<'_>],
        buf: &mut [u8],
    ) -> Option<usize> {
        Self::header(MSG_READDIR, request_id, STATUS_OK, buf)?;
        let mut off = RESP_HEADER_LEN + 2; // + count(u16)
        if off > buf.len() {
            return None;
        }
        let mut count: u16 = 0;
        for entry in entries {
            let name = entry.name.as_bytes();
            if name.len() > crate::DIRENT_NAME_MAX {
                return None;
            }
            let entry_len = 2 + name.len();
            if off + entry_len > buf.len() {
                return None;
            }
            buf[off] = name.len() as u8;
            buf[off + 1] = entry.is_dir as u8;
            buf[off + 2..off + 2 + name.len()].copy_from_slice(name);
            off += entry_len;
            count += 1;
        }
        buf[RESP_HEADER_LEN..RESP_HEADER_LEN + 2].copy_from_slice(&count.to_le_bytes());
        Some(off)
    }

    /// Encode `ReadOk`.
    pub fn encode_read_ok(request_id: u64, data: &[u8], buf: &mut [u8]) -> Option<usize> {
        let total = RESP_HEADER_LEN + 4 + data.len();
        if total > buf.len() {
            return None;
        }
        Self::header(MSG_READ, request_id, STATUS_OK, buf)?;
        buf[10..14].copy_from_slice(&(data.len() as u32).to_le_bytes());
        buf[14..14 + data.len()].copy_from_slice(data);
        Some(total)
    }

    /// Encode `WriteOk`.
    pub fn encode_write_ok(request_id: u64, written: u32, buf: &mut [u8]) -> Option<usize> {
        let total = RESP_HEADER_LEN + 4;
        if total > buf.len() {
            return None;
        }
        Self::header(MSG_WRITE, request_id, STATUS_OK, buf)?;
        buf[10..14].copy_from_slice(&written.to_le_bytes());
        Some(total)
    }

    /// Encode `CloseOk`.
    pub fn encode_close_ok(request_id: u64, buf: &mut [u8]) -> Option<usize> {
        let total = RESP_HEADER_LEN;
        if total > buf.len() {
            return None;
        }
        Self::header(MSG_CLOSE, request_id, STATUS_OK, buf)?;
        Some(total)
    }

    /// Encode `ConnectOk`. Header only — the channel travels as an attached
    /// handle on the message carrying this frame, not in the payload.
    pub fn encode_connect_ok(request_id: u64, buf: &mut [u8]) -> Option<usize> {
        let total = RESP_HEADER_LEN;
        if total > buf.len() {
            return None;
        }
        Self::header(MSG_CONNECT, request_id, STATUS_OK, buf)?;
        Some(total)
    }

    /// Encode `SizeOk`.
    pub fn encode_size_ok(request_id: u64, size: u64, buf: &mut [u8]) -> Option<usize> {
        let total = RESP_HEADER_LEN + 8;
        if total > buf.len() {
            return None;
        }
        Self::header(MSG_SIZE, request_id, STATUS_OK, buf)?;
        buf[10..18].copy_from_slice(&size.to_le_bytes());
        Some(total)
    }

    /// Encode `DisplayInfoOk`.
    pub fn encode_display_info_ok(
        request_id: u64,
        width: u32,
        height: u32,
        format: u32,
        stride: u32,
        buf: &mut [u8],
    ) -> Option<usize> {
        let total = RESP_HEADER_LEN + 16;
        if total > buf.len() {
            return None;
        }
        Self::header(MSG_DISPLAY_INFO, request_id, STATUS_OK, buf)?;
        buf[10..14].copy_from_slice(&width.to_le_bytes());
        buf[14..18].copy_from_slice(&height.to_le_bytes());
        buf[18..22].copy_from_slice(&format.to_le_bytes());
        buf[22..26].copy_from_slice(&stride.to_le_bytes());
        Some(total)
    }

    /// Encode `DisplayMapOk`.
    pub fn encode_display_map_ok(
        request_id: u64,
        phys_addr: u64,
        size: u64,
        buf: &mut [u8],
    ) -> Option<usize> {
        let total = RESP_HEADER_LEN + 16;
        if total > buf.len() {
            return None;
        }
        Self::header(MSG_DISPLAY_MAP, request_id, STATUS_OK, buf)?;
        buf[10..18].copy_from_slice(&phys_addr.to_le_bytes());
        buf[18..26].copy_from_slice(&size.to_le_bytes());
        Some(total)
    }

    /// Encode `DisplayFlushOk`.
    pub fn encode_display_flush_ok(request_id: u64, buf: &mut [u8]) -> Option<usize> {
        let total = RESP_HEADER_LEN;
        if total > buf.len() {
            return None;
        }
        Self::header(MSG_DISPLAY_FLUSH, request_id, STATUS_OK, buf)?;
        Some(total)
    }

    /// Encode `Notify` — see [`MSG_NOTIFY`]. Always uses `request_id == 0`.
    pub fn encode_notify(resource_id: u64, events: u32, buf: &mut [u8]) -> Option<usize> {
        let total = RESP_HEADER_LEN + 8 + 4;
        if total > buf.len() {
            return None;
        }
        Self::header(MSG_NOTIFY, 0, STATUS_OK, buf)?;
        buf[10..18].copy_from_slice(&resource_id.to_le_bytes());
        buf[18..22].copy_from_slice(&events.to_le_bytes());
        Some(total)
    }

    /// Decode a response frame from `buf`. Returns `None` on truncated or
    /// malformed input.
    pub fn decode(buf: &'a [u8]) -> Option<Response<'a>> {
        if buf.len() < RESP_HEADER_LEN {
            return None;
        }
        let kind = buf[0];
        let request_id = u64::from_le_bytes(buf[1..9].try_into().ok()?);
        let status = buf[9];
        if kind == MSG_NOTIFY {
            let resource_id = u64::from_le_bytes(buf.get(10..18)?.try_into().ok()?);
            let events = u32::from_le_bytes(buf.get(18..22)?.try_into().ok()?);
            return Some(Response::Notify { resource_id, events });
        }
        if status == STATUS_ERR {
            let error = u8_to_error(*buf.get(10)?);
            return Some(match kind {
                MSG_OPEN => Response::OpenErr { request_id, error },
                MSG_READDIR => Response::ReaddirErr { request_id, error },
                MSG_READ => Response::ReadErr { request_id, error },
                MSG_WRITE => Response::WriteErr { request_id, error },
                MSG_CLOSE => Response::CloseErr { request_id, error },
                MSG_CONNECT => Response::ConnectErr { request_id, error },
                MSG_SIZE => Response::SizeErr { request_id, error },
                MSG_DISPLAY_INFO => Response::DisplayInfoErr { request_id, error },
                MSG_DISPLAY_MAP => Response::DisplayMapErr { request_id, error },
                MSG_DISPLAY_FLUSH => Response::DisplayFlushErr { request_id, error },
                _ => return None,
            });
        }
        if status != STATUS_OK {
            return None;
        }
        match kind {
            MSG_OPEN => {
                let resource_id = u64::from_le_bytes(buf.get(10..18)?.try_into().ok()?);
                Some(Response::OpenOk {
                    request_id,
                    resource_id,
                })
            }
            MSG_READDIR => Some(Response::ReaddirOk {
                request_id,
                raw: buf.get(RESP_HEADER_LEN..)?,
            }),
            MSG_READ => {
                let data_len = u32::from_le_bytes(buf.get(10..14)?.try_into().ok()?) as usize;
                let data = buf.get(14..14 + data_len)?;
                Some(Response::ReadOk { request_id, data })
            }
            MSG_WRITE => {
                let written = u32::from_le_bytes(buf.get(10..14)?.try_into().ok()?);
                Some(Response::WriteOk {
                    request_id,
                    written,
                })
            }
            MSG_CLOSE => Some(Response::CloseOk { request_id }),
            MSG_CONNECT => Some(Response::ConnectOk { request_id }),
            MSG_SIZE => {
                let size = u64::from_le_bytes(buf.get(10..18)?.try_into().ok()?);
                Some(Response::SizeOk { request_id, size })
            }
            MSG_DISPLAY_INFO => {
                let width = u32::from_le_bytes(buf.get(10..14)?.try_into().ok()?);
                let height = u32::from_le_bytes(buf.get(14..18)?.try_into().ok()?);
                let format = u32::from_le_bytes(buf.get(18..22)?.try_into().ok()?);
                let stride = u32::from_le_bytes(buf.get(22..26)?.try_into().ok()?);
                Some(Response::DisplayInfoOk { request_id, width, height, format, stride })
            }
            MSG_DISPLAY_MAP => {
                let phys_addr = u64::from_le_bytes(buf.get(10..18)?.try_into().ok()?);
                let size = u64::from_le_bytes(buf.get(18..26)?.try_into().ok()?);
                Some(Response::DisplayMapOk { request_id, phys_addr, size })
            }
            MSG_DISPLAY_FLUSH => Some(Response::DisplayFlushOk { request_id }),
            _ => None,
        }
    }
}

/// Iterator over the entries encoded by [`Response::encode_readdir_ok`],
/// parsing from the `raw` bytes carried by `Response::ReaddirOk` (the count
/// prefix followed by packed `(name_len, is_dir, name)` entries).
pub struct ReaddirEntriesIter<'a> {
    remaining: u16,
    buf: &'a [u8],
    off: usize,
}

impl<'a> ReaddirEntriesIter<'a> {
    /// `raw` is the `Response::ReaddirOk::raw` field.
    pub fn new(raw: &'a [u8]) -> Option<Self> {
        let count = u16::from_le_bytes(raw.get(0..2)?.try_into().ok()?);
        Some(Self {
            remaining: count,
            buf: raw,
            off: 2,
        })
    }
}

impl<'a> Iterator for ReaddirEntriesIter<'a> {
    type Item = ReaddirEntry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let name_len = *self.buf.get(self.off)? as usize;
        let is_dir = *self.buf.get(self.off + 1)? != 0;
        let name_start = self.off + 2;
        let name = core::str::from_utf8(self.buf.get(name_start..name_start + name_len)?).ok()?;
        self.off = name_start + name_len;
        self.remaining -= 1;
        Some(ReaddirEntry { name, is_dir })
    }
}
