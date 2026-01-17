#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    // Spawn the terminal emulator
    let Ok(_terminal_handle) = environment::spawn("file:/initrd/terminal") else {
        environment::log("init: failed to spawn terminal");
        return 1;
    };

    // Init's job is done - terminal will take over
    0
}
