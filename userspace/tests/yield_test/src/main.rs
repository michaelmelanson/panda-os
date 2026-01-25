#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::process;

libpanda::main! {
    environment::log("Parent: spawning child");
    let Ok(_) = environment::spawn("file:/initrd/yield_child", 0, 0) else {
        environment::log("Parent: FAIL - spawn failed");
        return 1;
    };

    for _ in 0..3 {
        environment::log("Parent: before yield");
        process::yield_now();
        environment::log("Parent: after yield");
    }

    environment::log("Parent: done");
    0
}
