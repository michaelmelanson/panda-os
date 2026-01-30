#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use libpanda::terminal::{self, Colour, NamedColour};
use libpanda::{environment, file};
use panda_abi::value::{Table, Value};

fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
    }
    ((hi as u64) << 32) | lo as u64
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        alloc::format!("{}", bytes)
    } else if bytes < 1024 * 1024 {
        alloc::format!("{}K", bytes / 1024)
    } else if bytes < 1024 * 1024 * 1024 {
        alloc::format!("{}M", bytes / (1024 * 1024))
    } else {
        alloc::format!("{}G", bytes / (1024 * 1024 * 1024))
    }
}

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
    let t0 = rdtsc();
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

    let t1 = rdtsc();

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

    let t2 = rdtsc();

    // Sort entries alphabetically
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Build table with Name, Type, and Size columns
    let headers = vec![
        Value::String(String::from("Name")),
        Value::String(String::from("Type")),
        Value::String(String::from("Size")),
    ];

    let mut cells: Vec<Value> = Vec::new();
    for (name, is_dir) in entries.iter() {
        // Get file size by stat'ing the entry
        let entry_path = if path.ends_with('/') {
            alloc::format!("file:{}{}", path, name)
        } else {
            alloc::format!("file:{}/{}", path, name)
        };
        let size = environment::stat(&entry_path)
            .map(|s| s.size)
            .unwrap_or(0);

        if *is_dir {
            // Directories in blue
            cells.push(terminal::coloured(name, Colour::Named(NamedColour::Blue)));
            cells.push(Value::String(String::from("dir")));
            cells.push(Value::String(String::from("-")));
        } else {
            cells.push(Value::String(name.clone()));
            cells.push(Value::String(String::from("file")));
            cells.push(Value::String(format_size(size)));
        }
    }

    let t3 = rdtsc();

    let table = Table::new(3, Some(headers), cells).unwrap();
    terminal::print_value(Value::Table(table));

    let t4 = rdtsc();

    environment::log(&alloc::format!(
        "[ls timing] opendir={} readdir={} stat+build={} print={}",
        t1 - t0, t2 - t1, t3 - t2, t4 - t3
    ));

    0
}
