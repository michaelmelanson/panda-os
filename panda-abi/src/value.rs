//! Universal value type for structured pipeline data.
//!
//! `Value` is the universal type for data flowing through pipelines. It provides:
//! - Primitives: Null, Bool, Int, Float, String, Bytes
//! - Containers: Array, Map
//! - Display modifiers: Styled, Link
//! - Structured display: Table
//!
//! # Control Plane vs Data Plane
//!
//! - **Data plane** (STDIN/STDOUT): `Value` objects flow between pipeline stages
//! - **Control plane** (PARENT): `Request`/`Event` messages for terminal interaction
//!
//! # Example
//!
//! ```
//! use panda_abi::value::{Value, Table, Style};
//!
//! // Simple text
//! let text = Value::String("Hello, world!".into());
//!
//! // Styled text
//! let styled = Value::Styled(Style::bold(), Box::new(Value::String("Important".into())));
//!
//! // Table
//! let table = Table::new(2, Some(vec![
//!     Value::String("Name".into()),
//!     Value::String("Size".into()),
//! ]), vec![
//!     Value::String("file.txt".into()), Value::Int(1024),
//!     Value::String("data.bin".into()), Value::Int(2048),
//! ]).unwrap();
//! ```

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::encoding::{Decode, DecodeError, Decoder, Encode, Encoder};
use crate::terminal::Style;

// =============================================================================
// Value type
// =============================================================================

/// Universal value type for pipeline data.
///
/// This is the core type exchanged between pipeline stages. It supports:
/// - Primitives for basic data types
/// - Containers for structured data (replaces JSON text encoding)
/// - Display modifiers that wrap any value (recursive styling)
/// - Tables for structured tabular output
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Null/nil/none value
    Null,
    /// Boolean value
    Bool(bool),
    /// 64-bit signed integer
    Int(i64),
    /// 64-bit floating point
    Float(f64),
    /// UTF-8 string (Unix text compatibility)
    String(String),
    /// Raw bytes (Unix binary compatibility)
    Bytes(Vec<u8>),
    /// Ordered array of values
    Array(Vec<Value>),
    /// String-keyed map (like JSON objects)
    Map(BTreeMap<String, Value>),
    /// Styled value (recursive - can wrap any Value)
    Styled(Style, Box<Value>),
    /// Hyperlink wrapping a value
    Link { url: String, inner: Box<Value> },
    /// Rectangular table with optional headers
    Table(Table),
}

/// Type tag for Value encoding
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueTag {
    Null = 0,
    Bool = 1,
    Int = 2,
    Float = 3,
    String = 4,
    Bytes = 5,
    Array = 6,
    Map = 7,
    Styled = 8,
    Link = 9,
    Table = 10,
}

impl ValueTag {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Null),
            1 => Some(Self::Bool),
            2 => Some(Self::Int),
            3 => Some(Self::Float),
            4 => Some(Self::String),
            5 => Some(Self::Bytes),
            6 => Some(Self::Array),
            7 => Some(Self::Map),
            8 => Some(Self::Styled),
            9 => Some(Self::Link),
            10 => Some(Self::Table),
            _ => None,
        }
    }
}

impl Encode for Value {
    fn encode(&self, enc: &mut Encoder) {
        match self {
            Value::Null => {
                enc.write_u8(ValueTag::Null as u8);
            }
            Value::Bool(b) => {
                enc.write_u8(ValueTag::Bool as u8);
                enc.write_bool(*b);
            }
            Value::Int(i) => {
                enc.write_u8(ValueTag::Int as u8);
                enc.write_i64(*i);
            }
            Value::Float(f) => {
                enc.write_u8(ValueTag::Float as u8);
                enc.write_f64(*f);
            }
            Value::String(s) => {
                enc.write_u8(ValueTag::String as u8);
                s.encode(enc);
            }
            Value::Bytes(b) => {
                enc.write_u8(ValueTag::Bytes as u8);
                enc.write_byte_array(b);
            }
            Value::Array(arr) => {
                enc.write_u8(ValueTag::Array as u8);
                arr.encode(enc);
            }
            Value::Map(map) => {
                enc.write_u8(ValueTag::Map as u8);
                enc.write_u16(map.len() as u16);
                for (k, v) in map {
                    k.encode(enc);
                    v.encode(enc);
                }
            }
            Value::Styled(style, inner) => {
                enc.write_u8(ValueTag::Styled as u8);
                style.encode(enc);
                inner.encode(enc);
            }
            Value::Link { url, inner } => {
                enc.write_u8(ValueTag::Link as u8);
                url.encode(enc);
                inner.encode(enc);
            }
            Value::Table(table) => {
                enc.write_u8(ValueTag::Table as u8);
                table.encode(enc);
            }
        }
    }
}

impl Decode for Value {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let tag = ValueTag::from_u8(dec.read_u8()?).ok_or(DecodeError::UnknownType)?;
        match tag {
            ValueTag::Null => Ok(Value::Null),
            ValueTag::Bool => Ok(Value::Bool(dec.read_bool()?)),
            ValueTag::Int => Ok(Value::Int(dec.read_i64()?)),
            ValueTag::Float => Ok(Value::Float(dec.read_f64()?)),
            ValueTag::String => Ok(Value::String(String::decode(dec)?)),
            ValueTag::Bytes => Ok(Value::Bytes(dec.read_byte_array()?)),
            ValueTag::Array => Ok(Value::Array(Vec::<Value>::decode(dec)?)),
            ValueTag::Map => {
                let count = dec.read_u16()? as usize;
                let mut map = BTreeMap::new();
                for _ in 0..count {
                    let k = String::decode(dec)?;
                    let v = Value::decode(dec)?;
                    map.insert(k, v);
                }
                Ok(Value::Map(map))
            }
            ValueTag::Styled => {
                let style = Style::decode(dec)?;
                let inner = Value::decode(dec)?;
                Ok(Value::Styled(style, Box::new(inner)))
            }
            ValueTag::Link => {
                let url = String::decode(dec)?;
                let inner = Value::decode(dec)?;
                Ok(Value::Link {
                    url,
                    inner: Box::new(inner),
                })
            }
            ValueTag::Table => Ok(Value::Table(Table::decode(dec)?)),
        }
    }
}

impl Value {
    /// Encode to bytes with a simple header (for channel messages).
    pub fn to_bytes(&self) -> Vec<u8> {
        Encode::to_bytes(self)
    }

    /// Decode from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        Decode::from_bytes(bytes)
    }

    /// Create a styled value with bold text.
    pub fn bold(inner: Value) -> Self {
        Value::Styled(Style::bold(), Box::new(inner))
    }

    /// Create a styled value with a foreground color.
    pub fn colored(inner: Value, color: crate::terminal::Colour) -> Self {
        Value::Styled(Style::fg(color), Box::new(inner))
    }
}

// =============================================================================
// Table type
// =============================================================================

/// Rectangular table with optional headers.
///
/// The table enforces rectangular structure:
/// - `headers` (if Some) must have exactly `cols` elements
/// - `cells` must have a length that is a multiple of `cols`
///
/// Cells are stored in row-major order.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    /// Number of columns
    pub cols: u16,
    /// Optional header row (length must equal cols)
    pub headers: Option<Vec<Value>>,
    /// Cell data in row-major order (length must be multiple of cols)
    pub cells: Vec<Value>,
}

/// Error when constructing an invalid table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableError {
    /// Headers length doesn't match column count
    InvalidHeaderCount,
    /// Cells length is not a multiple of column count
    InvalidCellCount,
    /// Column count is zero
    ZeroColumns,
}

impl Table {
    /// Create a new table with validation.
    ///
    /// Returns an error if:
    /// - `cols` is 0
    /// - `headers` length doesn't equal `cols`
    /// - `cells` length is not a multiple of `cols`
    pub fn new(
        cols: u16,
        headers: Option<Vec<Value>>,
        cells: Vec<Value>,
    ) -> Result<Self, TableError> {
        if cols == 0 {
            return Err(TableError::ZeroColumns);
        }
        if let Some(ref h) = headers {
            if h.len() != cols as usize {
                return Err(TableError::InvalidHeaderCount);
            }
        }
        if cells.len() % cols as usize != 0 {
            return Err(TableError::InvalidCellCount);
        }
        Ok(Self {
            cols,
            headers,
            cells,
        })
    }

    /// Create an empty table with the given column count and optional headers.
    pub fn with_headers(headers: Vec<Value>) -> Result<Self, TableError> {
        let cols = headers.len() as u16;
        if cols == 0 {
            return Err(TableError::ZeroColumns);
        }
        Ok(Self {
            cols,
            headers: Some(headers),
            cells: Vec::new(),
        })
    }

    /// Get the number of rows (excluding headers).
    pub fn rows(&self) -> usize {
        if self.cols == 0 {
            0
        } else {
            self.cells.len() / self.cols as usize
        }
    }

    /// Get a cell by row and column index.
    pub fn get(&self, row: usize, col: u16) -> Option<&Value> {
        if col < self.cols && row < self.rows() {
            Some(&self.cells[row * self.cols as usize + col as usize])
        } else {
            None
        }
    }

    /// Iterate over rows as slices.
    pub fn row_iter(&self) -> impl Iterator<Item = &[Value]> {
        self.cells.chunks(self.cols as usize)
    }

    /// Add a row to the table.
    ///
    /// Panics if `row.len() != self.cols`.
    pub fn push_row(&mut self, row: Vec<Value>) {
        assert_eq!(
            row.len(),
            self.cols as usize,
            "row length must equal column count"
        );
        self.cells.extend(row);
    }
}

impl Encode for Table {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u16(self.cols);
        self.headers.encode(enc);
        self.cells.encode(enc);
    }
}

impl Decode for Table {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let cols = dec.read_u16()?;
        let headers = Option::<Vec<Value>>::decode(dec)?;
        let cells = Vec::<Value>::decode(dec)?;

        // Validate structure
        if cols == 0 && !cells.is_empty() {
            return Err(DecodeError::InvalidValue);
        }
        if let Some(ref h) = headers {
            if h.len() != cols as usize {
                return Err(DecodeError::InvalidValue);
            }
        }
        if cols > 0 && cells.len() % cols as usize != 0 {
            return Err(DecodeError::InvalidValue);
        }

        Ok(Self {
            cols,
            headers,
            cells,
        })
    }
}

// =============================================================================
// Style helpers
// =============================================================================

impl Style {
    /// Create a bold style.
    pub fn bold() -> Self {
        Self {
            bold: true,
            ..Default::default()
        }
    }

    /// Create a style with foreground color.
    pub fn fg(color: crate::terminal::Colour) -> Self {
        Self {
            foreground: Some(color),
            ..Default::default()
        }
    }

    /// Create a style with foreground and background colors.
    pub fn colors(fg: crate::terminal::Colour, bg: crate::terminal::Colour) -> Self {
        Self {
            foreground: Some(fg),
            background: Some(bg),
            ..Default::default()
        }
    }

    /// Create an italic style.
    pub fn italic() -> Self {
        Self {
            italic: true,
            ..Default::default()
        }
    }

    /// Create an underlined style.
    pub fn underline() -> Self {
        Self {
            underline: true,
            ..Default::default()
        }
    }

    /// Combine this style with another (self takes precedence for conflicts).
    pub fn merge(&self, other: &Style) -> Style {
        Style {
            foreground: self.foreground.or(other.foreground),
            background: self.background.or(other.background),
            bold: self.bold || other.bold,
            italic: self.italic || other.italic,
            underline: self.underline || other.underline,
            strikethrough: self.strikethrough || other.strikethrough,
        }
    }
}
