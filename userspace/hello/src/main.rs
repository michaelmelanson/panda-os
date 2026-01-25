#![no_std]
#![no_main]

use libpanda::terminal;

libpanda::main! {
    terminal::println("Hello, world!");
    0
}
