//! Diagnostic logging for the filesystem service.
//!
//! The moved-from-kernel code (`crate::ext2`) used the `log` crate, which
//! the kernel wires up to its serial logger. Userspace has no `log`
//! implementation, so `log::warn!` there would compile to nothing and
//! silently discard every on-disk-corruption diagnostic ext2 emits. These
//! two macros keep those diagnostics by routing them to
//! `libpanda::environment::log` (the process-level equivalent, which the
//! kernel prints as `LOG:` on the serial console), and compile to nothing
//! on the host, where the `os` feature is off and there is no kernel to log
//! to.

/// Log a warning. See the module doc comment.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        $crate::logging::log_line(::alloc::format!($($arg)*).as_str())
    };
}

/// Log an error. See the module doc comment.
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => {
        $crate::logging::log_line(::alloc::format!($($arg)*).as_str())
    };
}

#[cfg(feature = "os")]
pub fn log_line(message: &str) {
    libpanda::environment::log(message);
}

#[cfg(not(feature = "os"))]
pub fn log_line(_message: &str) {}
