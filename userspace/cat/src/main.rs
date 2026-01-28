#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use libpanda::io::{File, Read};
use libpanda::stdio::output_value;
use libpanda::terminal;
use panda_abi::value::Value;

libpanda::main! { |args|
    if args.len() < 2 {
        terminal::error("Usage: cat <file>");
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
            terminal::error(&alloc::format!("cat: {}: No such file or directory", path));
            return 1;
        }
    };

    // Read entire file into buffer
    let mut contents = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break, // EOF
            Ok(n) => n,
            Err(_) => {
                terminal::error("cat: error reading file");
                return 1;
            }
        };
        contents.extend_from_slice(&buf[..n]);
    }

    // Output as Value::String if valid UTF-8, otherwise Value::Bytes
    let value = match String::from_utf8(contents.clone()) {
        Ok(s) => Value::String(s),
        Err(_) => Value::Bytes(contents),
    };

    if let Err(_) = output_value(&value) {
        terminal::error("cat: error writing output");
        return 1;
    }

    0
}
