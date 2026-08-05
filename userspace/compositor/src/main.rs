#![no_std]
#![no_main]

libpanda::main! {
    compositor::server::run(None);
    0
}
