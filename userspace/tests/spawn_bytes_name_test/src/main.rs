#![no_std]
#![no_main]

use libpanda::environment;

// Deliberately not a valid ELF image, so the kernel's spawn_from_elf error
// path runs and logs a label for it.
const GARBAGE: &[u8] = b"this is not an ELF image";

libpanda::main! {
    environment::log("Spawn bytes name test: starting");

    match environment::spawn_bytes("spawn_bytes_name_test:garbage-image", GARBAGE) {
        Ok(_) => {
            environment::log("FAIL: spawn_bytes unexpectedly succeeded");
            return 1;
        }
        Err(_) => {
            environment::log("Spawn bytes name test: spawn_bytes failed as expected");
        }
    }

    environment::log("PASS");
    0
}
