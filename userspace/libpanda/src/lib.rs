//! Panda userspace library.
//!
//! This crate provides abstractions for userspace programs running on Panda OS.
//!
//! ## Module organisation
//!
//! The library is organised into two layers:
//!
//! ### Low-level (`sys::*`)
//!
//! The `sys` module contains thin, zero-cost syscall wrappers that return raw
//! `isize` error codes. Use these when you need maximum control or performance.
//!
//! ### High-level (root modules)
//!
//! The root-level modules (`file`, `process`, `environment`, etc.) provide
//! ergonomic APIs with `Result` types and RAII wrappers.

#![no_std]
#![feature(alloc_error_handler)]

extern crate alloc;

// Low-level syscall wrappers
pub mod sys;

// Core types
pub mod error;
pub mod graphics;
pub mod io;
pub mod ipc;

// High-level modules (these use sys:: internally)
pub mod buffer;
pub mod channel;
pub mod environment;
pub mod file;
pub mod handle;
pub mod heap;
pub mod keyboard;
pub mod mailbox;
pub mod print;
pub mod process;
pub mod startup;
pub mod stdio;
pub mod terminal;

// Re-export alloc types for convenience
pub use alloc::{boxed::Box, format, string::String, vec, vec::Vec};

// Re-export core types
pub use handle::Handle;

// Re-export ABI types
pub use panda_abi::DirEntry;

/// Special exit codes used by the runtime.
pub mod exit_code {
    /// Exit code for unhandled panic.
    pub const PANIC: i32 = 101;
    /// Exit code for allocation failure (out of memory).
    pub const ALLOC_ERROR: i32 = 102;
}

#[alloc_error_handler]
fn alloc_error_handler(_layout: core::alloc::Layout) -> ! {
    environment::log("ALLOC ERROR: out of memory");
    process::exit(exit_code::ALLOC_ERROR);
}

/// Entry point macro for userspace programs.
///
/// Handles the `_start` symbol, panic handler, and program exit.
/// Automatically receives startup arguments from the parent process.
///
/// Use `|args|` to access command-line arguments:
///
/// # Example
/// ```
/// // Without args
/// libpanda::main! {
///     environment::log("Hello!");
///     0
/// }
///
/// // With args
/// libpanda::main! { |args|
///     for arg in &args {
///         environment::log(arg);
///     }
///     0
/// }
/// ```
#[macro_export]
macro_rules! main {
    (|$args:ident| $($body:tt)*) => {
        $crate::__main_impl!($args, $($body)*);
    };
    ($($body:tt)*) => {
        $crate::__main_impl!(_args, $($body)*);
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! __main_impl {
    ($args:ident, $($body:tt)*) => {
        #[unsafe(no_mangle)]
        extern "C" fn _start() -> ! {
            // Receive startup arguments from parent
            #[allow(unused_variables)]
            let $args: $crate::Vec<$crate::String> = $crate::startup::receive_args();
            let exit_code = (|| -> i32 { $($body)* })();
            $crate::process::exit(exit_code);
        }

        #[panic_handler]
        fn panic(info: &core::panic::PanicInfo) -> ! {
            use core::fmt::Write;

            // Buffer for formatting panic info with internal storage
            struct LogBuffer {
                buf: [u8; 256],
                pos: usize,
            }

            impl LogBuffer {
                fn new() -> Self {
                    Self { buf: [0; 256], pos: 0 }
                }

                fn flush(&mut self) {
                    if self.pos > 0 {
                        if let Ok(s) = core::str::from_utf8(&self.buf[..self.pos]) {
                            $crate::environment::log(s);
                        }
                        self.pos = 0;
                    }
                }
            }

            impl Write for LogBuffer {
                fn write_str(&mut self, s: &str) -> core::fmt::Result {
                    for b in s.bytes() {
                        if self.pos >= self.buf.len() {
                            self.flush();
                        }
                        self.buf[self.pos] = b;
                        self.pos += 1;
                    }
                    Ok(())
                }
            }

            impl Drop for LogBuffer {
                fn drop(&mut self) {
                    self.flush();
                }
            }

            let mut w = LogBuffer::new();

            // Print location first (most useful info)
            if let Some(location) = info.location() {
                let _ = write!(w, "PANIC at {}:{}: ", location.file(), location.line());
            } else {
                let _ = write!(w, "PANIC: ");
            }

            // Print the panic message
            let _ = write!(w, "{}", info.message());

            drop(w);

            $crate::process::exit($crate::exit_code::PANIC);
        }
    };
}
