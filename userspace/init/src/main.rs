#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    environment::log("HELLO FROM USERSPACE");
    0
}
