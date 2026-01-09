#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::process;

libpanda::main! {
    for _ in 0..3 {
        environment::log("Child: before yield");
        process::yield_now();
        environment::log("Child: after yield");
    }
    environment::log("Child: done");
    0
}
