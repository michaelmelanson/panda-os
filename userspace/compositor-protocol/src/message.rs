//! The compositor wire protocol.
//!
//! Frames are hand-rolled little-endian binary, in the same style as
//! `panda_abi::scheme_protocol`: a one-byte tag followed by fixed-width
//! fields. Every frame fits in a single channel message — pixels never
//! travel over the channel, only buffer handles attached to
//! [`Request::AttachBuffer`].
//!
//! Requests are not correlated by id: a client's requests are processed in
//! the order it sent them, so the reply to the *n*th `CreateWindow` is the
//! *n*th `WindowCreated`.

use crate::Rect;

/// BGRA byte order, 8 bits per channel (little-endian ARGB8888) — the only
/// format the compositor accepts today.
pub const FORMAT_BGRA8888: u8 = 1;

const TAG_CREATE_WINDOW: u8 = 1;
const TAG_ATTACH_BUFFER: u8 = 2;
const TAG_DAMAGE: u8 = 3;
const TAG_COMMIT: u8 = 4;
const TAG_FILL: u8 = 5;
const TAG_SET_VISIBLE: u8 = 6;
const TAG_MOVE: u8 = 7;
const TAG_DESTROY_WINDOW: u8 = 8;

const TAG_DISPLAY_FORMATS: u8 = 1;
const TAG_WINDOW_CREATED: u8 = 2;
const TAG_FRAME_DONE: u8 = 3;
const TAG_BUFFER_RELEASED: u8 = 4;
const TAG_CLOSED: u8 = 5;

/// Upper bound on the format list in a `DisplayFormats` greeting.
pub const MAX_FORMATS: usize = 16;

/// `Fill` — tag, window, rect, colour — is the longest fixed-size frame.
const LONGEST_FIXED_FRAME: usize = 1 + 8 + 16 + 4;
/// `DisplayFormats` is the only variable-length frame, bounded by
/// [`MAX_FORMATS`].
const LONGEST_VARIABLE_FRAME: usize = 1 + 4 + 4 + 1 + MAX_FORMATS;

/// The largest frame any message in this protocol encodes to, so callers can
/// size a buffer without guessing.
pub const MAX_FRAME_SIZE: usize = if LONGEST_FIXED_FRAME > LONGEST_VARIABLE_FRAME {
    LONGEST_FIXED_FRAME
} else {
    LONGEST_VARIABLE_FRAME
};

fn u32_at(buf: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        buf.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn u64_at(buf: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        buf.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn put_rect(buf: &mut [u8], offset: usize, rect: Rect) {
    buf[offset..offset + 4].copy_from_slice(&rect.x.to_le_bytes());
    buf[offset + 4..offset + 8].copy_from_slice(&rect.y.to_le_bytes());
    buf[offset + 8..offset + 12].copy_from_slice(&rect.width.to_le_bytes());
    buf[offset + 12..offset + 16].copy_from_slice(&rect.height.to_le_bytes());
}

fn rect_at(buf: &[u8], offset: usize) -> Option<Rect> {
    Some(Rect {
        x: u32_at(buf, offset)?,
        y: u32_at(buf, offset + 4)?,
        width: u32_at(buf, offset + 8)?,
        height: u32_at(buf, offset + 12)?,
    })
}

/// A message sent by a client to the compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Allocate a window. Answered with [`Event::WindowCreated`].
    CreateWindow,
    /// Attach the shared buffer carried as this message's handle attachment
    /// to `window`. Resizing a window is an `AttachBuffer` with new
    /// dimensions. The compositor names each attachment by a per-window
    /// sequence number, starting at 0, which both sides can count and which
    /// [`Event::BufferReleased`] reports back.
    AttachBuffer {
        window: u64,
        width: u32,
        height: u32,
        format: u8,
    },
    /// Mark a window-relative region as changed.
    Damage { window: u64, rect: Rect },
    /// Latch the attached buffer and accumulated damage for the next frame.
    Commit { window: u64 },
    /// Fill a window-relative region with a solid BGRA colour.
    Fill {
        window: u64,
        rect: Rect,
        colour: u32,
    },
    /// Show or hide a window.
    SetVisible { window: u64, visible: bool },
    /// Move a window's top-left corner to a screen position.
    Move { window: u64, x: u32, y: u32 },
    /// Destroy a window and release its buffers.
    DestroyWindow { window: u64 },
}

impl Request {
    /// Encode into `buf`, returning the frame length, or `None` if `buf` is
    /// too small.
    pub fn encode(self, buf: &mut [u8]) -> Option<usize> {
        match self {
            Request::CreateWindow => {
                *buf.first_mut()? = TAG_CREATE_WINDOW;
                Some(1)
            }
            Request::AttachBuffer {
                window,
                width,
                height,
                format,
            } => {
                let total = 1 + 8 + 4 + 4 + 1;
                if buf.len() < total {
                    return None;
                }
                buf[0] = TAG_ATTACH_BUFFER;
                buf[1..9].copy_from_slice(&window.to_le_bytes());
                buf[9..13].copy_from_slice(&width.to_le_bytes());
                buf[13..17].copy_from_slice(&height.to_le_bytes());
                buf[17] = format;
                Some(total)
            }
            Request::Damage { window, rect } => {
                let total = 1 + 8 + 16;
                if buf.len() < total {
                    return None;
                }
                buf[0] = TAG_DAMAGE;
                buf[1..9].copy_from_slice(&window.to_le_bytes());
                put_rect(buf, 9, rect);
                Some(total)
            }
            Request::Commit { window } => {
                let total = 1 + 8;
                if buf.len() < total {
                    return None;
                }
                buf[0] = TAG_COMMIT;
                buf[1..9].copy_from_slice(&window.to_le_bytes());
                Some(total)
            }
            Request::Fill {
                window,
                rect,
                colour,
            } => {
                let total = 1 + 8 + 16 + 4;
                if buf.len() < total {
                    return None;
                }
                buf[0] = TAG_FILL;
                buf[1..9].copy_from_slice(&window.to_le_bytes());
                put_rect(buf, 9, rect);
                buf[25..29].copy_from_slice(&colour.to_le_bytes());
                Some(total)
            }
            Request::SetVisible { window, visible } => {
                let total = 1 + 8 + 1;
                if buf.len() < total {
                    return None;
                }
                buf[0] = TAG_SET_VISIBLE;
                buf[1..9].copy_from_slice(&window.to_le_bytes());
                buf[9] = visible as u8;
                Some(total)
            }
            Request::Move { window, x, y } => {
                let total = 1 + 8 + 4 + 4;
                if buf.len() < total {
                    return None;
                }
                buf[0] = TAG_MOVE;
                buf[1..9].copy_from_slice(&window.to_le_bytes());
                buf[9..13].copy_from_slice(&x.to_le_bytes());
                buf[13..17].copy_from_slice(&y.to_le_bytes());
                Some(total)
            }
            Request::DestroyWindow { window } => {
                let total = 1 + 8;
                if buf.len() < total {
                    return None;
                }
                buf[0] = TAG_DESTROY_WINDOW;
                buf[1..9].copy_from_slice(&window.to_le_bytes());
                Some(total)
            }
        }
    }

    /// Decode a frame. Returns `None` on a truncated or unknown frame — a
    /// client is untrusted input, so nothing is guessed.
    pub fn decode(buf: &[u8]) -> Option<Request> {
        match *buf.first()? {
            TAG_CREATE_WINDOW => Some(Request::CreateWindow),
            TAG_ATTACH_BUFFER => Some(Request::AttachBuffer {
                window: u64_at(buf, 1)?,
                width: u32_at(buf, 9)?,
                height: u32_at(buf, 13)?,
                format: *buf.get(17)?,
            }),
            TAG_DAMAGE => Some(Request::Damage {
                window: u64_at(buf, 1)?,
                rect: rect_at(buf, 9)?,
            }),
            TAG_COMMIT => Some(Request::Commit {
                window: u64_at(buf, 1)?,
            }),
            TAG_FILL => Some(Request::Fill {
                window: u64_at(buf, 1)?,
                rect: rect_at(buf, 9)?,
                colour: u32_at(buf, 25)?,
            }),
            TAG_SET_VISIBLE => Some(Request::SetVisible {
                window: u64_at(buf, 1)?,
                visible: *buf.get(9)? != 0,
            }),
            TAG_MOVE => Some(Request::Move {
                window: u64_at(buf, 1)?,
                x: u32_at(buf, 9)?,
                y: u32_at(buf, 13)?,
            }),
            TAG_DESTROY_WINDOW => Some(Request::DestroyWindow {
                window: u64_at(buf, 1)?,
            }),
            _ => None,
        }
    }
}

/// A message sent by the compositor to a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event<'a> {
    /// Sent once when a client connects: the screen geometry it will be
    /// composited onto and the pixel formats its buffers may use.
    DisplayFormats {
        width: u32,
        height: u32,
        formats: &'a [u8],
    },
    /// Answers [`Request::CreateWindow`].
    WindowCreated { window: u64 },
    /// The tick that consumed a commit has completed.
    FrameDone { window: u64, frame: u64 },
    /// The compositor will no longer read that attached buffer, so the
    /// client may render into it again.
    BufferReleased { window: u64, buffer: u64 },
    /// The window is gone (destroyed by the client, or by the compositor).
    Closed { window: u64 },
}

impl<'a> Event<'a> {
    /// Encode into `buf`, returning the frame length, or `None` if `buf` is
    /// too small (or the format list exceeds [`MAX_FORMATS`]).
    pub fn encode(self, buf: &mut [u8]) -> Option<usize> {
        match self {
            Event::DisplayFormats {
                width,
                height,
                formats,
            } => {
                if formats.len() > MAX_FORMATS {
                    return None;
                }
                let total = 1 + 4 + 4 + 1 + formats.len();
                if buf.len() < total {
                    return None;
                }
                buf[0] = TAG_DISPLAY_FORMATS;
                buf[1..5].copy_from_slice(&width.to_le_bytes());
                buf[5..9].copy_from_slice(&height.to_le_bytes());
                buf[9] = formats.len() as u8;
                buf[10..total].copy_from_slice(formats);
                Some(total)
            }
            Event::WindowCreated { window } => Self::encode_one(buf, TAG_WINDOW_CREATED, window),
            Event::FrameDone { window, frame } => {
                Self::encode_two(buf, TAG_FRAME_DONE, window, frame)
            }
            Event::BufferReleased { window, buffer } => {
                Self::encode_two(buf, TAG_BUFFER_RELEASED, window, buffer)
            }
            Event::Closed { window } => Self::encode_one(buf, TAG_CLOSED, window),
        }
    }

    fn encode_one(buf: &mut [u8], tag: u8, value: u64) -> Option<usize> {
        let total = 1 + 8;
        if buf.len() < total {
            return None;
        }
        buf[0] = tag;
        buf[1..9].copy_from_slice(&value.to_le_bytes());
        Some(total)
    }

    fn encode_two(buf: &mut [u8], tag: u8, first: u64, second: u64) -> Option<usize> {
        let total = 1 + 8 + 8;
        if buf.len() < total {
            return None;
        }
        buf[0] = tag;
        buf[1..9].copy_from_slice(&first.to_le_bytes());
        buf[9..17].copy_from_slice(&second.to_le_bytes());
        Some(total)
    }

    /// Decode a frame, borrowing from `buf`. Returns `None` on a truncated
    /// or unknown frame.
    pub fn decode(buf: &'a [u8]) -> Option<Event<'a>> {
        match *buf.first()? {
            TAG_DISPLAY_FORMATS => {
                let count = *buf.get(9)? as usize;
                Some(Event::DisplayFormats {
                    width: u32_at(buf, 1)?,
                    height: u32_at(buf, 5)?,
                    formats: buf.get(10..10 + count)?,
                })
            }
            TAG_WINDOW_CREATED => Some(Event::WindowCreated {
                window: u64_at(buf, 1)?,
            }),
            TAG_FRAME_DONE => Some(Event::FrameDone {
                window: u64_at(buf, 1)?,
                frame: u64_at(buf, 9)?,
            }),
            TAG_BUFFER_RELEASED => Some(Event::BufferReleased {
                window: u64_at(buf, 1)?,
                buffer: u64_at(buf, 9)?,
            }),
            TAG_CLOSED => Some(Event::Closed {
                window: u64_at(buf, 1)?,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECT: Rect = Rect {
        x: 7,
        y: 11,
        width: 640,
        height: 480,
    };

    fn round_trip_request(request: Request) {
        let mut buf = [0u8; MAX_FRAME_SIZE];
        let len = request.encode(&mut buf).expect("encode");
        assert_eq!(Request::decode(&buf[..len]), Some(request));
    }

    #[test]
    fn requests_round_trip() {
        round_trip_request(Request::CreateWindow);
        round_trip_request(Request::AttachBuffer {
            window: 3,
            width: 640,
            height: 480,
            format: FORMAT_BGRA8888,
        });
        round_trip_request(Request::Damage {
            window: 3,
            rect: RECT,
        });
        round_trip_request(Request::Commit { window: 3 });
        round_trip_request(Request::Fill {
            window: 3,
            rect: RECT,
            colour: 0xFF2E3440,
        });
        round_trip_request(Request::SetVisible {
            window: 3,
            visible: true,
        });
        round_trip_request(Request::SetVisible {
            window: 3,
            visible: false,
        });
        round_trip_request(Request::Move {
            window: 3,
            x: 100,
            y: 200,
        });
        round_trip_request(Request::DestroyWindow { window: 3 });
    }

    #[test]
    fn events_round_trip() {
        let mut buf = [0u8; MAX_FRAME_SIZE];
        let formats = [FORMAT_BGRA8888];
        for event in [
            Event::DisplayFormats {
                width: 1920,
                height: 1080,
                formats: &formats,
            },
            Event::WindowCreated { window: 5 },
            Event::FrameDone {
                window: 5,
                frame: 42,
            },
            Event::BufferReleased {
                window: 5,
                buffer: 1,
            },
            Event::Closed { window: 5 },
        ] {
            let len = event.encode(&mut buf).expect("encode");
            assert_eq!(Event::decode(&buf[..len]), Some(event));
        }
    }

    #[test]
    fn truncated_frames_are_rejected() {
        let mut buf = [0u8; MAX_FRAME_SIZE];
        let len = Request::Fill {
            window: 1,
            rect: RECT,
            colour: 0,
        }
        .encode(&mut buf)
        .expect("encode");
        for short in 0..len {
            assert_eq!(Request::decode(&buf[..short]), None, "len {short}");
        }

        let len = Event::FrameDone {
            window: 1,
            frame: 1,
        }
        .encode(&mut buf)
        .expect("encode");
        for short in 0..len {
            assert_eq!(Event::decode(&buf[..short]), None, "len {short}");
        }
    }

    #[test]
    fn unknown_tags_are_rejected() {
        assert_eq!(Request::decode(&[200, 0, 0, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(Event::decode(&[200, 0, 0, 0, 0, 0, 0, 0, 0]), None);
    }

    #[test]
    fn encoding_into_a_short_buffer_fails_rather_than_truncating() {
        let mut buf = [0u8; 4];
        assert_eq!(Request::Commit { window: 1 }.encode(&mut buf), None);
        assert_eq!(Event::Closed { window: 1 }.encode(&mut buf), None);
    }

    #[test]
    fn an_over_long_format_list_is_rejected() {
        let mut buf = [0u8; MAX_FRAME_SIZE * 2];
        let formats = [FORMAT_BGRA8888; MAX_FORMATS + 1];
        assert_eq!(
            Event::DisplayFormats {
                width: 1,
                height: 1,
                formats: &formats,
            }
            .encode(&mut buf),
            None
        );
    }

    #[test]
    fn a_display_formats_frame_claiming_more_formats_than_it_carries_is_rejected() {
        let mut frame = [0u8; 11];
        frame[0] = TAG_DISPLAY_FORMATS;
        frame[9] = 4;
        assert_eq!(Event::decode(&frame), None);
    }
}
