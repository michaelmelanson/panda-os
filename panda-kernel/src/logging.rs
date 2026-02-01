use core::fmt::{Result, Write};

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

pub fn _print(args: ::core::fmt::Arguments) {
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
