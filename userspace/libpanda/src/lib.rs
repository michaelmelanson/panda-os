#![no_std]
#![feature(alloc_error_handler)]

extern crate alloc;

pub mod environment;
pub mod file;
pub mod heap;
pub mod print;
pub mod process;
pub mod syscall;

// Re-export alloc types for convenience
pub use alloc::{boxed::Box, format, string::String, vec, vec::Vec};

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
            // Try to print panic location if available
            if let Some(location) = info.location() {
                // Format a simple message - we can't use format! in no_std
                $crate::environment::log("PANIC at ");
                $crate::environment::log(location.file());
            } else {
                $crate::environment::log("PANIC");
            }
            $crate::process::exit(101_i32);
        }
    };
}
