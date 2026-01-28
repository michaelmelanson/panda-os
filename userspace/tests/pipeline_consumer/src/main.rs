#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use libpanda::{environment, stdio};
use panda_abi::value::Value;

libpanda::main! {
    environment::log("pipeline_consumer: starting");

    let mut sum: i64 = 0;
    let mut count = 0;

    // Read all values from stdin until EOF
    loop {
        match stdio::read_value() {
            Ok(Some(Value::Int(n))) => {
                sum += n;
                count += 1;
            }
            Ok(Some(_)) => {
                environment::log("pipeline_consumer: unexpected value type");
                return 1;
            }
            Ok(None) | Err(_) => {
                // EOF or channel closed
                break;
            }
        }
    }

    environment::log(&format!("pipeline_consumer: received {} values, sum = {}", count, sum));

    // Output the result as a Value
    let result = Value::Int(sum);
    if stdio::output_value(&result).is_err() {
        environment::log("pipeline_consumer: output_value failed");
        return 1;
    }

    environment::log("pipeline_consumer: done");
    0
}
