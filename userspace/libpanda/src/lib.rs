#![no_std]
#![feature(alloc_error_handler)]

extern crate alloc;

pub mod buffer;
pub mod channel;
pub mod environment;
pub mod file;
pub mod handle;
pub mod heap;
pub mod print;
pub mod process;
pub mod syscall;

// Re-export alloc types for convenience
pub use alloc::{boxed::Box, format, string::String, vec, vec::Vec};

// Re-export core types
pub use handle::Handle;

// Re-export ABI types
pub use panda_abi::DirEntry;

#[alloc_error_handler]
fn alloc_error_handler(_layout: core::alloc::Layout) -> ! {
    environment::log("ALLOC ERROR: out of memory");
    process::exit(102_i32);
}

/// Entry point macro for userspace programs.
///
/// Handles the `_start` symbol, panic handler, and program exit.
///
/// # Example
/// ```
/// libpanda::main! {
///     syscall_log("Hello from userspace");
///     // return value becomes the exit code
///     0
/// }
/// ```
#[macro_export]
macro_rules! main {
    ($($body:tt)*) => {
        #[unsafe(no_mangle)]
        extern "C" fn _start() -> ! {
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

            $crate::process::exit(101_i32);
        }
    };
}
