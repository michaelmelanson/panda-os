//! Toy userspace scheme provider for `scheme_provider_test` (M2.2).
//!
//! Registers the "echo" scheme and serves it: a single file-like resource
//! (path "/echo") whose reads return whatever was last written, plus a
//! fixed directory listing at "/mydir". Any other path is `NotFound`.
//!
//! Coordinates with the parent over the ordinary parent/child channel (NOT
//! the scheme provider channel, which is a completely separate connection
//! to the kernel): sends "ready" once the scheme is registered, and exits
//! immediately (without replying to any further scheme requests) on
//! receiving "die" — this is what lets the parent test "provider exits
//! while client has an open handle" deterministically.

#![no_std]
#![no_main]

use libpanda::ipc::Channel;
use libpanda::scheme::SchemeProvider;
use libpanda::{environment, process};
use panda_abi::scheme_protocol::{ReaddirEntry, Request};
use panda_abi::{ErrorCode, MAX_MESSAGE_SIZE};

libpanda::main! {
    environment::log("scheme_provider_child: starting");

    let Some(parent) = Channel::parent() else {
        environment::log("FAIL: no parent channel");
        return 1;
    };

    let provider = match SchemeProvider::register("echo") {
        Ok(p) => p,
        Err(_) => {
            environment::log("FAIL: could not register echo scheme");
            return 1;
        }
    };
    environment::log("scheme_provider_child: registered echo scheme");

    if parent.send(b"ready").is_err() {
        environment::log("FAIL: could not signal ready to parent");
        return 1;
    }

    // resource_id 1 is the sole open-able file this toy provider serves;
    // its content is whatever was last written to it (echo semantics).
    let mut echo_data = [0u8; 256];
    let mut echo_len: usize = 0;

    let mut req_buf = [0u8; MAX_MESSAGE_SIZE];
    let mut parent_buf = [0u8; 64];
    loop {
        // Non-blocking check for the parent telling us to exit. Checked
        // ahead of any pending scheme request so the parent's "the still-
        // open handle's next request must fail once I'm gone" test is
        // deterministic: once "die" is seen, no more scheme requests are
        // served, even if one is already queued.
        if let Ok(Some(len)) = parent.try_recv(&mut parent_buf) {
            if &parent_buf[..len] == b"die" {
                environment::log(
                    "scheme_provider_child: told to exit, exiting without further replies",
                );
                return 0;
            }
        }

        match provider.try_recv(&mut req_buf) {
            Ok(Some(Request::Open { request_id, path })) => {
                if path == "/echo" {
                    let _ = provider.reply_open_ok(request_id, 1);
                } else {
                    let _ = provider.reply_open_err(request_id, ErrorCode::NotFound);
                }
            }
            Ok(Some(Request::Readdir { request_id, path })) => {
                if path == "/mydir" {
                    let entries = [
                        ReaddirEntry {
                            name: "a.txt",
                            is_dir: false,
                        },
                        ReaddirEntry {
                            name: "sub",
                            is_dir: true,
                        },
                    ];
                    let _ = provider.reply_readdir_ok(request_id, &entries);
                } else {
                    let _ = provider.reply_readdir_err(request_id, ErrorCode::NotFound);
                }
            }
            Ok(Some(Request::Read {
                request_id,
                resource_id,
                len,
            })) => {
                if resource_id == 1 {
                    let n = (len as usize).min(echo_len);
                    let _ = provider.reply_read_ok(request_id, &echo_data[..n]);
                } else {
                    let _ = provider.reply_read_err(request_id, ErrorCode::InvalidHandle);
                }
            }
            Ok(Some(Request::Write {
                request_id,
                resource_id,
                data,
            })) => {
                if resource_id == 1 {
                    let n = data.len().min(echo_data.len());
                    echo_data[..n].copy_from_slice(&data[..n]);
                    echo_len = n;
                    let _ = provider.reply_write_ok(request_id, n as u32);
                } else {
                    let _ = provider.reply_write_err(request_id, ErrorCode::InvalidHandle);
                }
            }
            Ok(Some(Request::Close { request_id, .. })) => {
                let _ = provider.reply_close_ok(request_id);
            }
            Ok(None) => {
                process::yield_now();
            }
            Err(_) => {
                environment::log("FAIL: provider recv/decode error");
                return 1;
            }
        }
    }
}
