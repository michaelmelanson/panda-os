#![no_std]
#![no_main]

use libpanda::{environment, stdio};
use panda_abi::value::Value;

libpanda::main! {
    environment::log("pipeline_producer: starting");

    // Output integers 1 through 10
    for i in 1..=10 {
        let value = Value::Int(i);
        if stdio::write_value(&value).is_err() {
            environment::log("pipeline_producer: write_value failed");
            return 1;
        }
    }

    environment::log("pipeline_producer: done, sent 10 values");
    0
}
