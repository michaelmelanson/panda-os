use core::fmt::{Result, Write};

use spinning_top::Spinlock;
use x86_64::instructions::interrupts;
use x86_64::instructions::port::Port;

struct SerialPortWriter(u16);

impl Write for SerialPortWriter {
    fn write_str(&mut self, s: &str) -> Result {
        let mut port = Port::new(self.0);

        for c in s.chars() {
            unsafe {
                port.write(c as u8);
            }
        }

        Ok(())
    }
}

/// Serializes access to the serial console so that concurrent writers
/// (e.g. a preempted process and the process/kernel code scheduled in
/// its place) can't interleave their output character-by-character.
static SERIAL_LOCK: Spinlock<()> = Spinlock::new(());

/// RAII guard that disables interrupts and holds `SERIAL_LOCK` for its
/// lifetime, restoring the previous interrupt state on drop.
///
/// Interrupts must be disabled for the duration of the critical section
/// (not just the lock held): on this single-core kernel, if a writer were
/// preempted while holding the lock, the next-scheduled process could spin
/// forever trying to acquire it, since nothing would ever preempt it back
/// to the original holder to release the lock. This mirrors the pattern in
/// `memory::write_protection::WriteProtectGuard`.
struct SerialGuard {
    interrupts_were_enabled: bool,
    _lock: spinning_top::guard::SpinlockGuard<'static, ()>,
}

impl SerialGuard {
    fn new() -> Self {
        let interrupts_were_enabled = interrupts::are_enabled();
        interrupts::disable();

        Self {
            interrupts_were_enabled,
            _lock: SERIAL_LOCK.lock(),
        }
    }
}

impl Drop for SerialGuard {
    fn drop(&mut self) {
        if self.interrupts_were_enabled {
            interrupts::enable();
        }
    }
}

pub fn _print(args: ::core::fmt::Arguments) {
    // Hold the lock (with interrupts disabled) across the entire formatted
    // write, since `write_fmt` may call `write_str` multiple times for a
    // single logical line (once per substitution boundary) and we want the
    // whole line to be atomic with respect to other writers.
    let _guard = SerialGuard::new();
    SerialPortWriter(0x3f8).write_fmt(args).unwrap();
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::logging::_print(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($fmt:expr) => {
        {
            $crate::print!($fmt);
            $crate::print!("\n");
        }
    };
    ($fmt:expr, $($arg:tt)*) => {
        {
            $crate::print!($fmt, $($arg)*);
            $crate::print!("\n");
        }
    };
}

pub struct Logger;
impl Logger {
    pub fn init(&self) {
        _print(format_args!("\x1b[0m"));
    }
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        // Suppress ERROR from tar-no-std: it logs at ERROR when it hits the
        // end-of-archive zero blocks, which is normal TAR behaviour.
        if metadata.target().starts_with("tar_no_std") && metadata.level() == log::Level::Error {
            return false;
        }
        true
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            println!(
                "[{}:{}] {}: {}",
                record.file().unwrap_or("unknown"),
                record.line().unwrap_or(0),
                record.level(),
                record.args()
            );
        }
    }

    fn flush(&self) {
        // nothing
    }
}
