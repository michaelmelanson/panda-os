#![no_std]
#![no_main]

use libpanda::{environment, file, ipc::Channel, process};

libpanda::main! {
    environment::log("Handle transfer test: starting");

    // Create a fresh channel pair. Endpoint B will be transferred to the
    // child over the parent<->child spawn channel; endpoint A stays here so
    // we can verify the transferred B is actually usable from the child.
    let Ok((handle_a, handle_b)) = libpanda::ipc::create_pair() else {
        environment::log("FAIL: create_pair failed");
        return 1;
    };
    let a = Channel::from_typed(handle_a);

    let Ok(child_handle) = environment::spawn("file:/initrd/handle_transfer_child") else {
        environment::log("FAIL: spawn returned error");
        return 1;
    };

    let Some(to_child) = Channel::from_handle_borrowed(child_handle.into()) else {
        environment::log("FAIL: child handle is not a channel");
        return 1;
    };

    // Plain message first: the child should see no attached handle on this
    // one (negative case, checked child-side).
    if to_child.send(b"plain, no handle").is_err() {
        environment::log("FAIL: sending plain message failed");
        return 1;
    }

    // Transfer endpoint B to the child alongside a message.
    if to_child
        .send_with_handle(b"channel B attached", handle_b.into())
        .is_err()
    {
        environment::log("FAIL: send_with_handle failed");
        return 1;
    }
    environment::log("Handle transfer test: sent transferred channel to child");

    // Close our own copy of B. Handle transfer duplicates the underlying
    // Arc rather than moving it (see docs/IPC.md "Handle transfer"), so the
    // child's copy must remain fully usable even after ours is gone.
    file::close(handle_b.into());

    // Wait for the child to reply over the transferred channel (via A).
    let mut buf = [0u8; 64];
    match a.recv(&mut buf) {
        Ok(len) if &buf[..len] == b"hello via transferred channel" => {
            environment::log("Handle transfer test: received message over transferred channel");
        }
        Ok(_) => {
            environment::log("FAIL: unexpected message over transferred channel");
            return 1;
        }
        Err(_) => {
            environment::log("FAIL: recv over transferred channel failed");
            return 1;
        }
    }

    let exit_code = process::wait(child_handle);
    if exit_code != 0 {
        environment::log("FAIL: child exited with non-zero code");
        return 1;
    }

    environment::log("Handle transfer test: child exited successfully");
    environment::log("PASS");
    0
}
