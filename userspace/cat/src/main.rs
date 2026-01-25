#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use libpanda::{environment, file};

libpanda::main! { |args|
    if args.len() < 2 {
        environment::log("Usage: cat <file>");
        return 1;
    }

    let path = &args[1];

    // Build the full URI
    let uri = if path.starts_with("file:") {
        String::from(path)
    } else {
        alloc::format!("file:{}", path)
    };

    // Open file
    let handle = match environment::open(&uri, 0, 0) {
        Ok(h) => h,
        Err(_) => {
            environment::log(&alloc::format!("cat: {}: No such file or directory", path));
            return 1;
        }
    };

    // Read and print contents
    let mut buf = [0u8; 512];
    loop {
        let n = file::read(handle, &mut buf);
        if n == 0 {
            break; // EOF
        }
        if n < 0 {
            environment::log("cat: error reading file");
            file::close(handle);
            return 1;
        }

        // Convert to string and log
        if let Ok(s) = core::str::from_utf8(&buf[..n as usize]) {
            // Log line by line to handle newlines properly
            for line in s.lines() {
                environment::log(line);
            }
        }
    }

    file::close(handle);
    0
}
