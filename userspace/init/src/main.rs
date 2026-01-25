#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    // Mount ext2 filesystem from the first block device
    if environment::mount("ext2", "/mnt").is_err() {
        environment::log("init: failed to mount ext2");
        return 1;
    }
    environment::log("init: mounted ext2 at /mnt");

    // Spawn the terminal emulator from ext2 filesystem
    let Ok(_terminal_handle) = environment::spawn("file:/mnt/terminal", &[], 0, 0) else {
        environment::log("init: failed to spawn terminal");
        return 1;
    };

    // Init's job is done - terminal will take over
    0
}
