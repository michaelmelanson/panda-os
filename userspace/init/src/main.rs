#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    // Spawn the shell
    let shell_pid = environment::spawn("file:/initrd/shell");
    if shell_pid < 0 {
        environment::log("init: failed to spawn shell");
        return 1;
    }

    // Init's job is done - shell will take over
    0
}
