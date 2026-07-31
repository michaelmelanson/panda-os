//! Userspace test for userspace scheme providers (M2.2).
//!
//! Spawns `scheme_provider_child`, a toy provider serving the "echo"
//! scheme, and exercises the client side of the protocol implemented in
//! `panda_abi::scheme_protocol` / `resource::scheme::UserSchemeProvider`:
//! open/write/read round-tripping, readdir, a clean error for an unknown
//! path, and a clean error (not a hang) on the client's next request after
//! the provider process has exited.

#![no_std]
#![no_main]

use libpanda::ipc::Channel;
use libpanda::{DirEntry, ErrorCode, String, Vec, environment, file, process};

libpanda::main! {
    environment::log("scheme_provider_test: starting");

    let Ok(child_handle) = environment::spawn("file:/initrd/scheme_provider_child") else {
        environment::log("FAIL: spawn returned error");
        return 1;
    };

    let Some(to_child) = Channel::from_handle_borrowed(child_handle.into()) else {
        environment::log("FAIL: child handle is not a channel");
        return 1;
    };

    // Wait for the child to register its scheme before touching it.
    let mut msg = [0u8; 64];
    match to_child.recv(&mut msg) {
        Ok(len) if &msg[..len] == b"ready" => {
            environment::log("scheme_provider_test: child registered echo scheme");
        }
        _ => {
            environment::log("FAIL: did not receive ready signal from child");
            return 1;
        }
    }

    // Test: open of an unknown path fails cleanly with NotFound.
    match environment::open("echo:/unknown", 0, 0) {
        Err(ErrorCode::NotFound) => {
            environment::log("scheme_provider_test: unknown path correctly refused with NotFound");
        }
        Err(_) => {
            environment::log("FAIL: open of unknown path failed with the wrong error");
            return 1;
        }
        Ok(_) => {
            environment::log("FAIL: open of unknown path unexpectedly succeeded");
            return 1;
        }
    }

    // Test: happy path -- open, write, read back (echo semantics).
    let Ok(handle) = environment::open("echo:/echo", 0, 0) else {
        environment::log("FAIL: could not open echo:/echo");
        return 1;
    };
    environment::log("scheme_provider_test: opened echo:/echo");

    let payload = b"hello scheme provider";
    let written = file::write(handle, payload);
    if written != payload.len() as isize {
        environment::log("FAIL: write to echo:/echo did not report the expected length");
        return 1;
    }

    let mut readback = [0u8; 64];
    let n = file::read(handle, &mut readback);
    if n < 0 || &readback[..n as usize] != payload {
        environment::log("FAIL: echo:/echo did not echo back the written data");
        return 1;
    }
    environment::log("scheme_provider_test: echo round-trip matched");

    // Test: readdir on a provider-served directory.
    let Ok(dir_handle) = environment::opendir("echo:/mydir") else {
        environment::log("FAIL: could not opendir echo:/mydir");
        return 1;
    };
    let mut names: Vec<String> = Vec::new();
    let mut entry = DirEntry {
        name_len: 0,
        is_dir: false,
        name: [0; 255],
    };
    loop {
        let result = file::readdir(dir_handle, &mut entry);
        if result < 0 {
            environment::log("FAIL: readdir on echo:/mydir returned an error");
            return 1;
        }
        if result == 0 {
            break;
        }
        names.push(String::from(entry.name()));
    }
    file::close(dir_handle);
    if !names.iter().any(|n| n.as_str() == "a.txt") || !names.iter().any(|n| n.as_str() == "sub")
    {
        environment::log("FAIL: echo:/mydir listing is missing expected entries");
        return 1;
    }
    environment::log("scheme_provider_test: readdir listing matched");

    // Tell the child to exit without serving any more requests, then
    // confirm it's actually gone before touching the still-open handle.
    if to_child.send(b"die").is_err() {
        environment::log("FAIL: could not signal child to die");
        return 1;
    }
    let exit_code = process::wait(child_handle);
    if exit_code != 0 {
        environment::log("FAIL: child exited with a non-zero code");
        return 1;
    }
    environment::log("scheme_provider_test: child exited");

    // Test: the provider is gone -- the still-open handle's next request
    // must fail cleanly, not hang or crash the kernel.
    let mut buf = [0u8; 16];
    let n = file::read(handle, &mut buf);
    if n >= 0 {
        environment::log("FAIL: read after provider exit unexpectedly succeeded");
        return 1;
    }
    environment::log("scheme_provider_test: read after provider exit failed cleanly as expected");

    file::close(handle);

    environment::log("PASS");
    0
}
