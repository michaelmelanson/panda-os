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

    // Spawn the compositor before any graphical client. It claims the
    // display and serves windows over the protocol; until the in-kernel
    // compositor is deleted (Phase 5 of plans/userspace-compositor.md) the
    // display is already claimed, so it comes up without output.
    let Ok(_compositor_handle) = environment::spawn("file:/mnt/compositor") else {
        environment::log("init: failed to spawn compositor");
        return 1;
    };

    // Spawn the terminal emulator from ext2 filesystem
    let Ok(_terminal_handle) = environment::spawn("file:/mnt/terminal") else {
        environment::log("init: failed to spawn terminal");
        return 1;
    };

    // Init's job is done - terminal will take over
    0
}
