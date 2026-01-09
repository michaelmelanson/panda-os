#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    environment::log("Spawn test: starting");

    // Spawn a child process
    let result = environment::spawn("/initrd/spawn_child");
    if result < 0 {
        environment::log("FAIL: spawn returned error");
        return 1;
    }

    environment::log("Spawn test: child spawned successfully");
    environment::log("Spawn test: parent exiting with code 0");
    0
}
