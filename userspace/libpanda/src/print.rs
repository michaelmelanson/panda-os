//! Print macros for userspace programs.
//!
//! Provides `print!` and `println!` macros that write to the parent channel.
//! For interactive terminal programs, the parent (terminal) receives this
//! output and displays it. For pipeline programs, output goes to stdout.

use core::fmt::{self, Write};

use crate::handle::Handle;
use crate::sys;

/// Buffer size for print output. Messages longer than this will be truncated.
const PRINT_BUFFER_SIZE: usize = 256;

/// A writer that buffers output and sends it to the parent channel.
struct OutputWriter {
    buffer: [u8; PRINT_BUFFER_SIZE],
    pos: usize,
}

impl OutputWriter {
    const fn new() -> Self {
        Self {
            buffer: [0; PRINT_BUFFER_SIZE],
            pos: 0,
        }
    }

    fn flush(&self) {
        if self.pos > 0 {
            // Send to parent channel (terminal or pipeline stage)
            let _ = sys::channel::send_msg(Handle::PARENT, &self.buffer[..self.pos]);
        }
    }
}

impl Write for OutputWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let available = PRINT_BUFFER_SIZE - self.pos;
        let to_copy = bytes.len().min(available);

        self.buffer[self.pos..self.pos + to_copy].copy_from_slice(&bytes[..to_copy]);
        self.pos += to_copy;

        Ok(())
    }
}

/// Internal function used by print macros.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    let mut writer = OutputWriter::new();
    let _ = writer.write_fmt(args);
    writer.flush();
}

/// Prints to the system log.
///
/// Equivalent to the `print!` macro in the standard library.
///
/// # Examples
///
/// ```
/// use libpanda::print;
///
/// print!("Hello, ");
/// print!("world!\n");
/// print!("The answer is {}\n", 42);
/// ```
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::print::_print(format_args!($($arg)*))
    };
}

/// Prints to the system log, with a newline.
///
/// Equivalent to the `println!` macro in the standard library.
///
/// # Examples
///
/// ```
/// use libpanda::println;
///
/// println!();
/// println!("Hello, world!");
/// println!("The answer is {}", 42);
/// ```
#[macro_export]
macro_rules! println {
    () => {
        $crate::print!("\n")
    };
    ($($arg:tt)*) => {
        $crate::print!("{}\n", format_args!($($arg)*))
    };
}
