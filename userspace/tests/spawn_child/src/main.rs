#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    environment::log("Child process running!");
    environment::log("Child process exiting with code 0");
    0
}
