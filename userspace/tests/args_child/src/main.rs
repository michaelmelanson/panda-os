#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! { |args|
    for arg in &args {
        environment::log(arg);
    }
    0
}
