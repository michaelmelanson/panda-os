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
    syscall::syscall_log("ALLOC ERROR: out of memory");
    syscall::syscall_exit(102);
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
            $crate::syscall::syscall_exit(exit_code as usize);
        }

        #[panic_handler]
        fn panic(info: &core::panic::PanicInfo) -> ! {
            // Try to print panic location if available
            if let Some(location) = info.location() {
                // Format a simple message - we can't use format! in no_std
                $crate::syscall::syscall_log("PANIC at ");
                $crate::syscall::syscall_log(location.file());
            } else {
                $crate::syscall::syscall_log("PANIC");
            }
            $crate::syscall::syscall_exit(101);
        }
    };
}
