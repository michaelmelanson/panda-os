#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    environment::log("Hello, world!");
    0
}
