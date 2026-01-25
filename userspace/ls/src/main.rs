#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use libpanda::{environment, file};

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
            environment::log(&alloc::format!("ls: cannot access '{}': No such file or directory", path));
            return 1;
        }
    };

    // Read and print entries
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
            environment::log("ls: error reading directory");
            file::close(dir);
            return 1;
        }

        let name = entry.name();
        if entry.is_dir {
            environment::log(&alloc::format!("{}/ ", name));
        } else {
            environment::log(&alloc::format!("{} ", name));
        }
    }

    file::close(dir);
    0
}
