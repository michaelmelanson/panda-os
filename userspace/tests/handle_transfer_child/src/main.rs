#![no_std]
#![no_main]

use libpanda::{environment, ipc::Channel};

libpanda::main! {
    environment::log("Handle transfer child: starting");

    let Some(parent) = Channel::parent() else {
        environment::log("FAIL: no parent channel");
        return 1;
    };

    // First message: plain send(), no attachment. Negative-case check: a
    // message with no attachment must report None, not a spurious handle.
    let mut buf = [0u8; 64];
    let (len, attached) = match parent.recv_with_handle(&mut buf) {
        Ok(result) => result,
        Err(_) => {
            environment::log("FAIL: recv_with_handle (plain message) failed");
            return 1;
        }
    };
    if attached.is_some() {
        environment::log("FAIL: plain message reported an attached handle");
        return 1;
    }
    if &buf[..len] != b"plain, no handle" {
        environment::log("FAIL: unexpected plain message payload");
        return 1;
    }
    environment::log("Handle transfer child: plain message had no handle, as expected");

    // Second message: carries the transferred channel endpoint B.
    let (len, attached) = match parent.recv_with_handle(&mut buf) {
        Ok(result) => result,
        Err(_) => {
            environment::log("FAIL: recv_with_handle (attachment) failed");
            return 1;
        }
    };
    if &buf[..len] != b"channel B attached" {
        environment::log("FAIL: unexpected attachment message payload");
        return 1;
    }
    let Some(handle_b) = attached else {
        environment::log("FAIL: expected a transferred handle");
        return 1;
    };
    environment::log("Handle transfer child: received transferred channel handle");

    let Some(b) = Channel::from_handle(handle_b) else {
        environment::log("FAIL: transferred handle is not a channel");
        return 1;
    };

    if b.send(b"hello via transferred channel").is_err() {
        environment::log("FAIL: send on transferred channel failed");
        return 1;
    }

    environment::log("Handle transfer child: sent message via transferred channel");
    0
}
