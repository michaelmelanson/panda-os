//! Toy userspace scheme provider for `scheme_provider_concurrency_test`
//! (regression coverage for the `AsyncLock` lost-wakeup bug).
//!
//! Registers the "conc" scheme and serves it: each `Open` gets its own
//! independent echo resource (a fresh `resource_id`, with its own buffer),
//! so several clients can round-trip through this provider without
//! stepping on each other's data.
//!
//! Deliberately stalls for a while (a bounded `yield_now()` spin) before
//! replying to `Open`, specifically to widen the window during which the
//! kernel's per-provider `AsyncLock` (see
//! `panda-kernel/src/resource/scheme.rs`) is held by one client's in-flight
//! round trip while other clients are concurrently trying to acquire it.
//! Without that widening, single-core cooperative scheduling might still
//! usually produce contention, but the delay makes it reliable rather than
//! timing-dependent.
//!
//! Coordinates with the parent over the ordinary parent/child channel (NOT
//! the scheme provider channel): sends "ready" once the scheme is
//! registered, and exits on receiving "die".

#![no_std]
#![no_main]

use libpanda::ipc::Channel;
use libpanda::scheme::SchemeProvider;
use libpanda::{environment, process};
use panda_abi::scheme_protocol::Request;
use panda_abi::{ErrorCode, MAX_MESSAGE_SIZE};

/// How many `yield_now()` calls to spin through before replying to an
/// `Open` request. Large enough that, with several clients racing to open
/// at once, at least two of them are reliably still waiting (either on the
/// channel receive or on the kernel's `AsyncLock`) when this loop is
/// running for the first opener.
const OPEN_DELAY_YIELDS: u32 = 500;

/// Up to this many concurrently-open resources; the test only ever opens a
/// handful at once.
const MAX_RESOURCES: usize = 8;

libpanda::main! {
    environment::log("scheme_provider_conc_provider_child: starting");

    let Some(parent) = Channel::parent() else {
        environment::log("FAIL: no parent channel");
        return 1;
    };

    let provider = match SchemeProvider::register("conc") {
        Ok(p) => p,
        Err(_) => {
            environment::log("FAIL: could not register conc scheme");
            return 1;
        }
    };
    environment::log("scheme_provider_conc_provider_child: registered conc scheme");

    if parent.send(b"ready").is_err() {
        environment::log("FAIL: could not signal ready to parent");
        return 1;
    }

    // Per-resource echo buffers, indexed by (resource_id - 1). resource_id
    // 0 is never assigned, so a request for resource_id 0 is always
    // InvalidHandle.
    let mut buffers: [[u8; 64]; MAX_RESOURCES] = [[0u8; 64]; MAX_RESOURCES];
    let mut lens: [usize; MAX_RESOURCES] = [0; MAX_RESOURCES];
    let mut next_resource_id: u64 = 1;

    let mut req_buf = [0u8; MAX_MESSAGE_SIZE];
    let mut parent_buf = [0u8; 64];
    loop {
        if let Ok(Some(len)) = parent.try_recv(&mut parent_buf) {
            if &parent_buf[..len] == b"die" {
                environment::log(
                    "scheme_provider_conc_provider_child: told to exit, exiting",
                );
                return 0;
            }
        }

        match provider.try_recv(&mut req_buf) {
            Ok(Some(Request::Open { request_id, path })) => {
                if path == "/echo" && (next_resource_id as usize) <= MAX_RESOURCES {
                    // Widen the AsyncLock contention window (see module doc).
                    for _ in 0..OPEN_DELAY_YIELDS {
                        process::yield_now();
                    }
                    let resource_id = next_resource_id;
                    next_resource_id += 1;
                    let _ = provider.reply_open_ok(request_id, resource_id);
                } else {
                    let _ = provider.reply_open_err(request_id, ErrorCode::NotFound);
                }
            }
            Ok(Some(Request::Read {
                request_id,
                resource_id,
                len,
            })) => {
                let idx = resource_id.wrapping_sub(1) as usize;
                if resource_id != 0 && idx < MAX_RESOURCES {
                    let n = (len as usize).min(lens[idx]);
                    let _ = provider.reply_read_ok(request_id, &buffers[idx][..n]);
                } else {
                    let _ = provider.reply_read_err(request_id, ErrorCode::InvalidHandle);
                }
            }
            Ok(Some(Request::Write {
                request_id,
                resource_id,
                data,
            })) => {
                let idx = resource_id.wrapping_sub(1) as usize;
                if resource_id != 0 && idx < MAX_RESOURCES {
                    let n = data.len().min(buffers[idx].len());
                    buffers[idx][..n].copy_from_slice(&data[..n]);
                    lens[idx] = n;
                    let _ = provider.reply_write_ok(request_id, n as u32);
                } else {
                    let _ = provider.reply_write_err(request_id, ErrorCode::InvalidHandle);
                }
            }
            Ok(Some(Request::Close { request_id, .. })) => {
                let _ = provider.reply_close_ok(request_id);
            }
            Ok(Some(Request::Readdir { request_id, .. })) => {
                let _ = provider.reply_readdir_err(request_id, ErrorCode::NotFound);
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
