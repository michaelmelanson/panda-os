#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use libpanda::io::{File, Read};
use libpanda::{print, println};

libpanda::main! { |args|
    if args.len() < 2 {
        println!("Usage: cat <file>");
        return 1;
    }

    let path = &args[1];

    // Build the full URI
    let uri = if path.starts_with("file:") {
        String::from(path)
    } else {
        alloc::format!("file:{}", path)
    };

    // Open file with RAII
    let mut file = match File::open(&uri) {
        Ok(f) => f,
        Err(_) => {
            println!("cat: {}: No such file or directory", path);
            return 1;
        }
    };

    // Read and print contents
    let mut buf = [0u8; 512];
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break, // EOF
            Ok(n) => n,
            Err(_) => {
                println!("cat: error reading file");
                return 1;
            }
        };

        // Print file contents
        if let Ok(s) = core::str::from_utf8(&buf[..n]) {
            print!("{}", s);
        }
    }

    // File is automatically closed here via Drop
    0
}
