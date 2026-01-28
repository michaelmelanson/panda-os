#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use libpanda::{channel, environment, process, process::ChildBuilder};
use panda_abi::MAX_MESSAGE_SIZE;
use panda_abi::terminal::Request;
use panda_abi::value::Value;

libpanda::main! {
    environment::log("control_plane_test: starting");

    // Spawn child process
    let Ok(child) = ChildBuilder::new("file:/initrd/control_plane_child")
        .args(&["control_plane_child"])
        .spawn_handle()
    else {
        environment::log("FAIL: failed to spawn child");
        return 1;
    };
    environment::log(&format!("control_plane_test: spawned child, handle={}", child.as_raw()));

    let mut buf = [0u8; MAX_MESSAGE_SIZE];
    let mut received_write = false;
    let mut received_error = false;
    let mut received_warning = false;

    // Read messages from child using blocking recv
    // We expect exactly 3 messages: Write, Error, Warning
    for _ in 0..3 {
        match channel::recv(child, &mut buf) {
            Ok(len) if len > 0 => {
                // Try to parse as Request
                match Request::from_bytes(&buf[..len]) {
                    Ok((Request::Write(value), _)) => {
                        environment::log("control_plane_test: received Write request");
                        if let Value::String(s) = value {
                            if s == "hello from child" {
                                environment::log("control_plane_test: Write content correct");
                                received_write = true;
                            } else {
                                environment::log(&format!("FAIL: unexpected Write content: {}", s));
                                return 1;
                            }
                        } else {
                            environment::log("FAIL: Write value is not a String");
                            return 1;
                        }
                    }
                    Ok((Request::Error(value), _)) => {
                        environment::log("control_plane_test: received Error request");
                        if let Value::String(s) = value {
                            if s == "test error message" {
                                environment::log("control_plane_test: Error content correct");
                                received_error = true;
                            } else {
                                environment::log(&format!("FAIL: unexpected Error content: {}", s));
                                return 1;
                            }
                        } else {
                            environment::log("FAIL: Error value is not a String");
                            return 1;
                        }
                    }
                    Ok((Request::Warning(value), _)) => {
                        environment::log("control_plane_test: received Warning request");
                        if let Value::String(s) = value {
                            if s == "test warning message" {
                                environment::log("control_plane_test: Warning content correct");
                                received_warning = true;
                            } else {
                                environment::log(&format!("FAIL: unexpected Warning content: {}", s));
                                return 1;
                            }
                        } else {
                            environment::log("FAIL: Warning value is not a String");
                            return 1;
                        }
                    }
                    Ok((other, _)) => {
                        environment::log(&format!("control_plane_test: received other request: {:?}", other));
                    }
                    Err(_) => {
                        environment::log("control_plane_test: failed to parse Request");
                    }
                }
            }
            Ok(_) => {
                environment::log("control_plane_test: received empty message");
            }
            Err(_) => {
                environment::log("control_plane_test: channel error");
                break;
            }
        }
    }

    // Wait for child to exit
    let exit_code = process::wait(child);
    environment::log(&format!("control_plane_test: child exited with code {}", exit_code));

    if exit_code != 0 {
        environment::log("FAIL: child exited with non-zero code");
        return 1;
    }

    if !received_write {
        environment::log("FAIL: did not receive Write request");
        return 1;
    }
    if !received_error {
        environment::log("FAIL: did not receive Error request");
        return 1;
    }
    if !received_warning {
        environment::log("FAIL: did not receive Warning request");
        return 1;
    }

    environment::log("PASS");
    0
}
