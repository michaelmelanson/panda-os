#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use libpanda::terminal::{self, Colour, NamedColour};
use libpanda::{environment, file};
use panda_abi::value::Value;

libpanda::main! { |args|
    // Default to current directory (root of mounted fs)
    let path = if args.len() > 1 {
        &args[1]
    } else {
        "/mnt"
    };

    // Build the full URI
    let uri = if path.starts_with("file:") {
        String::from(path)
    } else {
        alloc::format!("file:{}", path)
    };

    // Open directory
    let dir = match environment::opendir(&uri) {
        Ok(d) => d,
        Err(_) => {
            terminal::error(&alloc::format!(
                "ls: cannot access '{}': No such file or directory",
                path
            ));
            return 1;
        }
    };

    // Read entries into a vector first
    let mut entries: Vec<(String, bool)> = Vec::new();
    let mut entry = panda_abi::DirEntry {
        name_len: 0,
        is_dir: false,
        name: [0u8; panda_abi::DIRENT_NAME_MAX],
    };

    loop {
        let result = file::readdir(dir, &mut entry);
        if result == 0 {
            break; // End of directory
        }
        if result < 0 {
            terminal::error("ls: error reading directory");
            file::close(dir);
            return 1;
        }

        entries.push((String::from(entry.name()), entry.is_dir));
    }

    file::close(dir);

    // Sort entries alphabetically
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Build output as a list of Values
    let mut parts: Vec<Value> = Vec::new();
    for (i, (name, is_dir)) in entries.iter().enumerate() {
        if *is_dir {
            // Directories in blue with trailing slash
            parts.push(terminal::coloured(
                &alloc::format!("{}/", name),
                Colour::Named(NamedColour::Blue),
            ));
        } else {
            parts.push(Value::String(name.clone()));
        }

        // Add spacing between entries
        if i < entries.len() - 1 {
            parts.push(Value::String(String::from("  ")));
        }
    }
    parts.push(Value::String(String::from("\n")));

    // Print each part
    for part in parts {
        terminal::print_value(part);
    }

    0
}
