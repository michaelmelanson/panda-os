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
    Read { request_id: u64, resource_id: u64, len: u32 },
    Write { request_id: u64, resource_id: u64, data: &'a [u8] },
    Close { request_id: u64, resource_id: u64 },
    Connect { request_id: u64, path: &'a str },
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
            | Request::Connect { request_id, .. } => request_id,
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
                len,
            } => {
                let total = HEADER_LEN + 8 + 4;
                if total > buf.len() {
                    return None;
                }
                buf[0] = MSG_READ;
                buf[1..9].copy_from_slice(&request_id.to_le_bytes());
                buf[9..17].copy_from_slice(&resource_id.to_le_bytes());
                buf[17..21].copy_from_slice(&len.to_le_bytes());
                Some(total)
            }
            Request::Write {
                request_id,
                resource_id,
                data,
            } => {
                let total = HEADER_LEN + 8 + 4 + data.len();
                if total > buf.len() {
                    return None;
                }
                buf[0] = MSG_WRITE;
                buf[1..9].copy_from_slice(&request_id.to_le_bytes());
                buf[9..17].copy_from_slice(&resource_id.to_le_bytes());
                buf[17..21].copy_from_slice(&(data.len() as u32).to_le_bytes());
                buf[21..21 + data.len()].copy_from_slice(data);
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
                if buf.len() < 21 {
                    return None;
                }
                let resource_id = u64::from_le_bytes(buf[9..17].try_into().ok()?);
                let len = u32::from_le_bytes(buf[17..21].try_into().ok()?);
                Some(Request::Read {
                    request_id,
                    resource_id,
                    len,
                })
            }
            MSG_WRITE => {
                if buf.len() < 21 {
                    return None;
                }
                let resource_id = u64::from_le_bytes(buf[9..17].try_into().ok()?);
                let data_len = u32::from_le_bytes(buf[17..21].try_into().ok()?) as usize;
                let end = 21usize.checked_add(data_len)?;
                let data = buf.get(21..end)?;
                Some(Request::Write {
                    request_id,
                    resource_id,
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
            | Response::ConnectErr { request_id, .. } => request_id,
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

    /// Decode a response frame from `buf`. Returns `None` on truncated or
    /// malformed input.
    pub fn decode(buf: &'a [u8]) -> Option<Response<'a>> {
        if buf.len() < RESP_HEADER_LEN {
            return None;
        }
        let kind = buf[0];
        let request_id = u64::from_le_bytes(buf[1..9].try_into().ok()?);
        let status = buf[9];
        if status == STATUS_ERR {
            let error = u8_to_error(*buf.get(10)?);
            return Some(match kind {
                MSG_OPEN => Response::OpenErr { request_id, error },
                MSG_READDIR => Response::ReaddirErr { request_id, error },
                MSG_READ => Response::ReadErr { request_id, error },
                MSG_WRITE => Response::WriteErr { request_id, error },
                MSG_CLOSE => Response::CloseErr { request_id, error },
                MSG_CONNECT => Response::ConnectErr { request_id, error },
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
