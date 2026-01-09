#![no_std]
pub mod syscall;

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
