//! Rendering utilities for the terminal.
//!
//! This module provides text measurement, colour conversion, and word iteration
//! for line wrapping.

use panda_abi::terminal::{Colour, NamedColour};

// =============================================================================
// Word iterator for line wrapping
// =============================================================================

/// A word or separator in text for line wrapping purposes.
pub enum Word<'a> {
    /// A newline character
    Newline,
    /// Whitespace (spaces, tabs)
    Whitespace(&'a str),
    /// A word (non-whitespace text)
    Text(&'a str),
}

/// Iterator that splits text into words, whitespace, and newlines.
pub struct WordIter<'a> {
    remaining: &'a str,
}

impl<'a> WordIter<'a> {
    pub fn new(s: &'a str) -> Self {
        Self { remaining: s }
    }
}

impl<'a> Iterator for WordIter<'a> {
    type Item = Word<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining.is_empty() {
            return None;
        }

        // Check for newline
        if self.remaining.starts_with('\n') {
            self.remaining = &self.remaining[1..];
            return Some(Word::Newline);
        }

        // Check for whitespace run
        let ws_end = self
            .remaining
            .find(|c: char| c == '\n' || !c.is_whitespace())
            .unwrap_or(self.remaining.len());

        if ws_end > 0 {
            let ws = &self.remaining[..ws_end];
            self.remaining = &self.remaining[ws_end..];
            return Some(Word::Whitespace(ws));
        }

        // Find end of word (next whitespace or newline)
        let word_end = self
            .remaining
            .find(|c: char| c.is_whitespace())
            .unwrap_or(self.remaining.len());

        let word = &self.remaining[..word_end];
        self.remaining = &self.remaining[word_end..];
        Some(Word::Text(word))
    }
}

// =============================================================================
// Colour conversion
// =============================================================================

/// Convert a terminal Colour to ARGB u32
pub fn colour_to_argb(colour: &Colour) -> u32 {
    match colour {
        Colour::Named(named) => match named {
            NamedColour::Black => 0xFF000000,
            NamedColour::Red => 0xFFCD3131,
            NamedColour::Green => 0xFF0DBC79,
            NamedColour::Yellow => 0xFFE5E510,
            NamedColour::Blue => 0xFF2472C8,
            NamedColour::Magenta => 0xFFBC3FBC,
            NamedColour::Cyan => 0xFF11A8CD,
            NamedColour::White => 0xFFE5E5E5,
            NamedColour::BrightBlack => 0xFF666666,
            NamedColour::BrightRed => 0xFFF14C4C,
            NamedColour::BrightGreen => 0xFF23D18B,
            NamedColour::BrightYellow => 0xFFF5F543,
            NamedColour::BrightBlue => 0xFF3B8EEA,
            NamedColour::BrightMagenta => 0xFFD670D6,
            NamedColour::BrightCyan => 0xFF29B8DB,
            NamedColour::BrightWhite => 0xFFFFFFFF,
        },
        Colour::Palette(idx) => {
            // Basic 256-colour palette approximation
            if *idx < 16 {
                // Use named colours for first 16
                let named = match idx {
                    0 => NamedColour::Black,
                    1 => NamedColour::Red,
                    2 => NamedColour::Green,
                    3 => NamedColour::Yellow,
                    4 => NamedColour::Blue,
                    5 => NamedColour::Magenta,
                    6 => NamedColour::Cyan,
                    7 => NamedColour::White,
                    8 => NamedColour::BrightBlack,
                    9 => NamedColour::BrightRed,
                    10 => NamedColour::BrightGreen,
                    11 => NamedColour::BrightYellow,
                    12 => NamedColour::BrightBlue,
                    13 => NamedColour::BrightMagenta,
                    14 => NamedColour::BrightCyan,
                    _ => NamedColour::BrightWhite,
                };
                colour_to_argb(&Colour::Named(named))
            } else if *idx < 232 {
                // 216 colour cube (6x6x6)
                let idx = idx - 16;
                let r = (idx / 36) * 51;
                let g = ((idx / 6) % 6) * 51;
                let b = (idx % 6) * 51;
                0xFF000000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
            } else {
                // Grayscale (24 shades)
                let grey = (idx - 232) * 10 + 8;
                0xFF000000 | ((grey as u32) << 16) | ((grey as u32) << 8) | (grey as u32)
            }
        }
        Colour::Rgb { r, g, b } => {
            0xFF000000 | ((*r as u32) << 16) | ((*g as u32) << 8) | (*b as u32)
        }
    }
}
