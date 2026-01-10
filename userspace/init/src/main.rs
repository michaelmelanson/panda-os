#![no_std]
#![no_main]

use libpanda::{environment, Box};

libpanda::main! {
    environment::log("HELLO FROM USERSPACE");
    environment::log("About to allocate Box");

    // Test heap allocation with Box
    let boxed = Box::new(42u64);
    environment::log("Box allocated");

    if *boxed == 42 {
        environment::log("Box allocation works!");
    }

    environment::log("All heap tests passed!");
    0
}
