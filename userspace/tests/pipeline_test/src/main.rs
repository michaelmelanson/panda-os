#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use libpanda::{
    channel,
    environment::{self, SpawnParams},
    file, process,
};
use panda_abi::terminal::Request;
use panda_abi::value::Value;
use panda_abi::{EVENT_PROCESS_EXITED, MAX_MESSAGE_SIZE};

libpanda::main! {
    environment::log("pipeline_test: starting");

    // Create a channel pair for the pipeline: producer -> consumer
    let (read_end, write_end) = channel::create_pair();
    environment::log(&format!(
        "pipeline_test: created channel pair: read={}, write={}",
        read_end.as_raw(),
        write_end.as_raw()
    ));

    // Spawn consumer FIRST with stdin connected to read_end
    // This ensures consumer is ready to receive before producer sends
    // stdout=0 means output goes to parent channel
    let Ok(consumer) = SpawnParams::new("file:/initrd/pipeline_consumer")
        .args(&["pipeline_consumer"])
        .stdin(read_end.as_raw())
        .spawn()
    else {
        environment::log("FAIL: failed to spawn consumer");
        return 1;
    };
    environment::log(&format!("pipeline_test: spawned consumer, handle={}", consumer.as_raw()));

    // Spawn producer with stdout connected to write_end
    // stdin=0 means no stdin redirection
    let Ok(producer) = SpawnParams::new("file:/initrd/pipeline_producer")
        .args(&["pipeline_producer"])
        .mailbox(0, EVENT_PROCESS_EXITED)
        .stdout(write_end.as_raw())
        .spawn()
    else {
        environment::log("FAIL: failed to spawn producer");
        return 1;
    };
    environment::log(&format!("pipeline_test: spawned producer, handle={}", producer.as_raw()));

    // Close our copy of write_end - the producer has its own copy via STDOUT
    // This is important: when the producer exits, its STDOUT handle is dropped,
    // but our copy of write_end keeps the channel open. We need to close it
    // so the consumer sees EOF when the producer is done.
    file::close(write_end);
    environment::log("pipeline_test: closed write_end");

    // Also close our copy of read_end - the consumer has it via STDIN
    file::close(read_end);
    environment::log("pipeline_test: closed read_end");

    // Wait for producer to exit
    let producer_exit = process::wait(producer);
    environment::log(&format!("pipeline_test: producer exited with code {}", producer_exit));
    if producer_exit != 0 {
        environment::log("FAIL: producer exited with non-zero code");
        return 1;
    }

    // Read result from consumer (it outputs via PARENT since stdout=0)
    // The consumer uses output_value() which sends Request::Write(value) to PARENT
    let mut buf = [0u8; MAX_MESSAGE_SIZE];
    match channel::recv(consumer, &mut buf) {
        Ok(len) if len > 0 => {
            // Decode as Request::Write containing the Value
            match Request::from_bytes(&buf[..len]) {
                Ok((Request::Write(Value::Int(sum)), _)) => {
                    environment::log(&format!("pipeline_test: received sum = {}", sum));
                    // Sum of 1..=10 is 55
                    if sum != 55 {
                        environment::log(&format!("FAIL: expected sum 55, got {}", sum));
                        return 1;
                    }
                }
                Ok((Request::Write(other), _)) => {
                    environment::log(&format!("FAIL: expected Int, got {:?}", other));
                    return 1;
                }
                Ok((other, _)) => {
                    environment::log(&format!("FAIL: expected Write request, got {:?}", other));
                    return 1;
                }
                Err(_) => {
                    environment::log("FAIL: failed to decode Request from consumer");
                    return 1;
                }
            }
        }
        Ok(_) => {
            environment::log("FAIL: received empty message from consumer");
            return 1;
        }
        Err(_) => {
            environment::log("FAIL: failed to receive from consumer");
            return 1;
        }
    }

    // Wait for consumer to exit
    let consumer_exit = process::wait(consumer);
    environment::log(&format!("pipeline_test: consumer exited with code {}", consumer_exit));
    if consumer_exit != 0 {
        environment::log("FAIL: consumer exited with non-zero code");
        return 1;
    }

    environment::log("PASS");
    0
}
