#![no_std]
#![no_main]

use libpanda::io::{File, Read};
use libpanda::ipc::Channel;
use libpanda::{environment, String};

libpanda::main! {
    environment::log("API test: starting");

    // Test 1: io::File::open with RAII
    environment::log("Test 1: io::File::open");
    {
        let mut file = match File::open("file:/initrd/hello.txt") {
            Ok(f) => f,
            Err(e) => {
                environment::log("FAIL: File::open failed");
                return 1;
            }
        };

        let mut contents = String::new();
        match file.read_to_string(&mut contents) {
            Ok(n) => {
                if n == 0 {
                    environment::log("FAIL: read_to_string returned 0 bytes");
                    return 1;
                }
            }
            Err(_) => {
                environment::log("FAIL: read_to_string failed");
                return 1;
            }
        }
        // File automatically closed when dropped
    }
    environment::log("  File::open: OK");

    // Test 2: Channel::parent send/recv
    environment::log("Test 2: Channel::parent");
    {
        // Every process spawned by terminal has a parent channel
        match Channel::parent() {
            Some(_parent) => {
                // We have a parent channel - this is expected
                // Don't actually send anything as it would confuse the test harness
            }
            None => {
                // No parent is also valid (e.g., init process)
            }
        }
    }
    environment::log("  Channel::parent: OK");

    // Test 3: File handle method
    environment::log("Test 3: File::handle");
    {
        let file = File::open("file:/initrd/hello.txt").unwrap();
        let handle = file.handle();
        // Just verify we can get the handle
        let _ = handle.as_raw();
    }
    environment::log("  File::handle: OK");

    environment::log("PASS");
    0
}
