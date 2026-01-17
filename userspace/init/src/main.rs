#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    // Spawn the shell
    let Ok(_shell_handle) = environment::spawn("file:/initrd/shell") else {
        environment::log("init: failed to spawn shell");
        return 1;
    };

    // Init's job is done - shell will take over
    0
}
