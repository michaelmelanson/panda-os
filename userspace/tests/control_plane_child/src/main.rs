#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use libpanda::environment;
use libpanda::terminal;
use panda_abi::value::Value;

libpanda::main! {
    environment::log("control_plane_child: starting");

    // Send a Write request with a test value
    let test_value = Value::String(String::from("hello from child"));
    terminal::print_value(test_value);
    environment::log("control_plane_child: sent Write request");

    // Send an Error request
    terminal::error("test error message");
    environment::log("control_plane_child: sent Error request");

    // Send a Warning request
    terminal::warning("test warning message");
    environment::log("control_plane_child: sent Warning request");

    environment::log("control_plane_child: done");
    0
}
