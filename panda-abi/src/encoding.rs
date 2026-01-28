//! Generic TLV (type-length-value) encoding utilities.
//!
//! This module provides reusable primitives for serialising structured data
//! in a simple, no_std-compatible format.
//!
//! # Format
//!
//! Messages use a TLV header followed by payload:
//! ```text
//! +----------+----------+-------------+
//! | Type(u16)| Len(u32) | Payload ... |
//! +----------+----------+-------------+
//! ```
//!
//! Nested values use length-prefixed encoding:
//! - Strings: `len(u16) + utf8_bytes`
//! - Arrays: `count(u16) + elements`

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// Decode error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Not enough bytes in buffer
    Truncated,
    /// Unknown type discriminant
    UnknownType,
    /// Invalid value for field
    InvalidValue,
    /// Invalid UTF-8 in string
    InvalidUtf8,
}

/// A buffer for encoding messages.
///
/// Wraps a `Vec<u8>` and provides methods for writing primitives.
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    /// Create a new encoder.
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Create an encoder with pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity),
        }
    }

    /// Get the encoded bytes.
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }

    /// Get the current length.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Get a reference to the underlying buffer.
    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    /// Get a mutable reference to the underlying buffer.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.buf
    }

    /// Write a TLV header. Returns the position of the length field
    /// so it can be updated later with `update_length`.
    pub fn write_tlv_header(&mut self, msg_type: u16, length: u32) -> usize {
        self.buf.extend_from_slice(&msg_type.to_le_bytes());
        let len_pos = self.buf.len();
        self.buf.extend_from_slice(&length.to_le_bytes());
        len_pos
    }

    /// Update a previously written length field.
    pub fn update_length(&mut self, len_pos: usize, length: u32) {
        self.buf[len_pos..len_pos + 4].copy_from_slice(&length.to_le_bytes());
    }

    /// Write a u8.
    pub fn write_u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    /// Write a u16 (little-endian).
    pub fn write_u16(&mut self, value: u16) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    /// Write a u32 (little-endian).
    pub fn write_u32(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    /// Write an i32 (little-endian).
    pub fn write_i32(&mut self, value: i32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    /// Write an i64 (little-endian).
    pub fn write_i64(&mut self, value: i64) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    /// Write an f64 (little-endian).
    pub fn write_f64(&mut self, value: f64) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    /// Write a length-prefixed string (u16 length + utf8 bytes).
    pub fn write_string(&mut self, s: &str) {
        let bytes = s.as_bytes();
        self.write_u16(bytes.len() as u16);
        self.buf.extend_from_slice(bytes);
    }

    /// Write raw bytes.
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Write a length-prefixed byte array (u32 length + bytes).
    pub fn write_byte_array(&mut self, bytes: &[u8]) {
        self.write_u32(bytes.len() as u32);
        self.buf.extend_from_slice(bytes);
    }

    /// Write a bool as a single byte.
    pub fn write_bool(&mut self, value: bool) {
        self.buf.push(if value { 1 } else { 0 });
    }
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

/// A buffer for decoding messages.
///
/// Wraps a byte slice and provides methods for reading primitives.
pub struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    /// Create a new decoder from a byte slice.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Get the current position.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Get remaining bytes.
    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    /// Check if we've reached the end.
    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    /// Get a slice of remaining bytes.
    pub fn remaining_slice(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    /// Read a TLV header. Returns (type, length).
    pub fn read_tlv_header(&mut self) -> Result<(u16, u32), DecodeError> {
        let msg_type = self.read_u16()?;
        let length = self.read_u32()?;
        Ok((msg_type, length))
    }

    /// Read a u8.
    pub fn read_u8(&mut self) -> Result<u8, DecodeError> {
        if self.remaining() < 1 {
            return Err(DecodeError::Truncated);
        }
        let value = self.buf[self.pos];
        self.pos += 1;
        Ok(value)
    }

    /// Read a u16 (little-endian).
    pub fn read_u16(&mut self) -> Result<u16, DecodeError> {
        if self.remaining() < 2 {
            return Err(DecodeError::Truncated);
        }
        let value = u16::from_le_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(value)
    }

    /// Read a u32 (little-endian).
    pub fn read_u32(&mut self) -> Result<u32, DecodeError> {
        if self.remaining() < 4 {
            return Err(DecodeError::Truncated);
        }
        let value = u32::from_le_bytes([
            self.buf[self.pos],
            self.buf[self.pos + 1],
            self.buf[self.pos + 2],
            self.buf[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(value)
    }

    /// Read an i32 (little-endian).
    pub fn read_i32(&mut self) -> Result<i32, DecodeError> {
        if self.remaining() < 4 {
            return Err(DecodeError::Truncated);
        }
        let value = i32::from_le_bytes([
            self.buf[self.pos],
            self.buf[self.pos + 1],
            self.buf[self.pos + 2],
            self.buf[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(value)
    }

    /// Read an i64 (little-endian).
    pub fn read_i64(&mut self) -> Result<i64, DecodeError> {
        if self.remaining() < 8 {
            return Err(DecodeError::Truncated);
        }
        let value = i64::from_le_bytes([
            self.buf[self.pos],
            self.buf[self.pos + 1],
            self.buf[self.pos + 2],
            self.buf[self.pos + 3],
            self.buf[self.pos + 4],
            self.buf[self.pos + 5],
            self.buf[self.pos + 6],
            self.buf[self.pos + 7],
        ]);
        self.pos += 8;
        Ok(value)
    }

    /// Read an f64 (little-endian).
    pub fn read_f64(&mut self) -> Result<f64, DecodeError> {
        if self.remaining() < 8 {
            return Err(DecodeError::Truncated);
        }
        let value = f64::from_le_bytes([
            self.buf[self.pos],
            self.buf[self.pos + 1],
            self.buf[self.pos + 2],
            self.buf[self.pos + 3],
            self.buf[self.pos + 4],
            self.buf[self.pos + 5],
            self.buf[self.pos + 6],
            self.buf[self.pos + 7],
        ]);
        self.pos += 8;
        Ok(value)
    }

    /// Read a length-prefixed string (u16 length + utf8 bytes).
    pub fn read_string(&mut self) -> Result<String, DecodeError> {
        let len = self.read_u16()? as usize;
        if self.remaining() < len {
            return Err(DecodeError::Truncated);
        }
        let s = core::str::from_utf8(&self.buf[self.pos..self.pos + len])
            .map_err(|_| DecodeError::InvalidUtf8)?;
        self.pos += len;
        Ok(String::from(s))
    }

    /// Read a fixed number of bytes.
    pub fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        if self.remaining() < len {
            return Err(DecodeError::Truncated);
        }
        let bytes = &self.buf[self.pos..self.pos + len];
        self.pos += len;
        Ok(bytes)
    }

    /// Read a length-prefixed byte array (u32 length + bytes).
    pub fn read_byte_array(&mut self) -> Result<Vec<u8>, DecodeError> {
        let len = self.read_u32()? as usize;
        if self.remaining() < len {
            return Err(DecodeError::Truncated);
        }
        let bytes = self.buf[self.pos..self.pos + len].to_vec();
        self.pos += len;
        Ok(bytes)
    }

    /// Read a bool from a single byte.
    pub fn read_bool(&mut self) -> Result<bool, DecodeError> {
        Ok(self.read_u8()? != 0)
    }

    /// Skip a number of bytes.
    pub fn skip(&mut self, len: usize) -> Result<(), DecodeError> {
        if self.remaining() < len {
            return Err(DecodeError::Truncated);
        }
        self.pos += len;
        Ok(())
    }
}

/// Trait for types that can be encoded to bytes.
pub trait Encode {
    /// Encode this value to the encoder.
    fn encode(&self, enc: &mut Encoder);

    /// Convenience method to encode to a new Vec.
    fn to_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        self.encode(&mut enc);
        enc.finish()
    }
}

/// Trait for types that can be decoded from bytes.
pub trait Decode: Sized {
    /// Decode this value from the decoder.
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError>;

    /// Convenience method to decode from a byte slice.
    fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut dec = Decoder::new(bytes);
        Self::decode(&mut dec)
    }
}

// Implement Encode/Decode for primitive types

impl Encode for u8 {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u8(*self);
    }
}

impl Decode for u8 {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        dec.read_u8()
    }
}

impl Encode for u16 {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u16(*self);
    }
}

impl Decode for u16 {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        dec.read_u16()
    }
}

impl Encode for u32 {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u32(*self);
    }
}

impl Decode for u32 {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        dec.read_u32()
    }
}

impl Encode for i32 {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_i32(*self);
    }
}

impl Decode for i32 {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        dec.read_i32()
    }
}

impl Encode for bool {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_bool(*self);
    }
}

impl Decode for bool {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        dec.read_bool()
    }
}

impl Encode for String {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_string(self);
    }
}

impl Decode for String {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        dec.read_string()
    }
}

impl Encode for &str {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_string(self);
    }
}

impl<T: Encode> Encode for Vec<T> {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u16(self.len() as u16);
        for item in self {
            item.encode(enc);
        }
    }
}

impl<T: Decode> Decode for Vec<T> {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let count = dec.read_u16()? as usize;
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            items.push(T::decode(dec)?);
        }
        Ok(items)
    }
}

impl<T: Encode> Encode for Option<T> {
    fn encode(&self, enc: &mut Encoder) {
        match self {
            Some(value) => {
                enc.write_u8(1);
                value.encode(enc);
            }
            None => {
                enc.write_u8(0);
            }
        }
    }
}

impl<T: Decode> Decode for Option<T> {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let tag = dec.read_u8()?;
        if tag == 0 {
            Ok(None)
        } else {
            Ok(Some(T::decode(dec)?))
        }
    }
}

impl<A: Encode, B: Encode> Encode for (A, B) {
    fn encode(&self, enc: &mut Encoder) {
        self.0.encode(enc);
        self.1.encode(enc);
    }
}

impl<A: Decode, B: Decode> Decode for (A, B) {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let a = A::decode(dec)?;
        let b = B::decode(dec)?;
        Ok((a, b))
    }
}
