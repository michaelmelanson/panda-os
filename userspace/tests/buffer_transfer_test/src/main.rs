#![no_std]
#![no_main]

use libpanda::buffer::Buffer;
use libpanda::{environment, ipc::Channel, process};

/// Buffer size shared by parent and child (no shared header exists between
/// the two binaries, so both sides just hardcode the same value).
const BUFFER_SIZE: usize = 4096;

/// Deterministic pattern the parent writes before transferring the buffer,
/// so the child can verify it received the SAME physical memory rather
/// than a fresh zeroed buffer.
fn pattern_byte(i: usize) -> u8 {
    (i % 256) as u8
}

/// Marker byte the child overwrites the whole buffer with, so the parent
/// can verify the child's writes — made through the child's own,
/// independent `OP_BUFFER_MAP` mapping in the child's own address space —
/// are visible back in the parent's mapping: true shared memory in both
/// directions, not a copy.
const CHILD_MARKER: u8 = 0x77;

libpanda::main! {
    environment::log("Buffer transfer test: starting");

    let Some(mut buf) = Buffer::alloc(BUFFER_SIZE) else {
        environment::log("FAIL: could not allocate buffer");
        return 1;
    };

    {
        let slice = buf.as_mut_slice();
        for i in 0..BUFFER_SIZE {
            slice[i] = pattern_byte(i);
        }
    }
    environment::log("Buffer transfer test: wrote pattern");

    let Ok(child_handle) = environment::spawn("file:/initrd/buffer_transfer_child") else {
        environment::log("FAIL: spawn returned error");
        return 1;
    };

    let Some(to_child) = Channel::from_handle_borrowed(child_handle.into()) else {
        environment::log("FAIL: child handle is not a channel");
        return 1;
    };

    // Transfer the buffer handle to the child. Handle transfer duplicates
    // the resource's Arc rather than moving it (see docs/SYSCALLS.md
    // "Handle transfer"), so `buf` remains fully valid in the parent
    // afterwards — it's still mapped at its original, alloc-time address.
    if to_child
        .send_with_handle(b"buffer attached", buf.handle())
        .is_err()
    {
        environment::log("FAIL: send_with_handle failed");
        return 1;
    }
    environment::log("Buffer transfer test: sent buffer handle to child");

    // Wait for the child to confirm: it received the handle, mapped the
    // buffer with OP_BUFFER_MAP (twice, to exercise the double-map
    // policy), verified our pattern, and overwrote the buffer with its
    // reply marker.
    let mut msg = [0u8; 64];
    match to_child.recv(&mut msg) {
        Ok(len) if &msg[..len] == b"child done" => {
            environment::log("Buffer transfer test: child signalled done");
        }
        Ok(_) => {
            environment::log("FAIL: unexpected message from child");
            return 1;
        }
        Err(_) => {
            environment::log("FAIL: recv from child failed");
            return 1;
        }
    }

    // Re-read OUR OWN mapping (the parent never calls OP_BUFFER_MAP itself
    // — this is still the alloc-time mapping) and verify the child's
    // writes are visible here.
    {
        let slice = buf.as_slice();
        if slice.iter().any(|&b| b != CHILD_MARKER) {
            environment::log("FAIL: child's writes not visible in parent's mapping");
            return 1;
        }
    }
    environment::log("Buffer transfer test: observed child's writes (shared memory confirmed)");

    let exit_code = process::wait(child_handle);
    if exit_code != 0 {
        environment::log("FAIL: child exited with non-zero code");
        return 1;
    }
    environment::log("Buffer transfer test: child exited successfully");

    // The child process — and its OP_BUFFER_MAP mapping(s) — are now fully
    // torn down. Prove process exit only tore down the CHILD's own
    // mapping(s), not the shared frames: write and read back a fresh
    // pattern through the parent's own mapping.
    {
        let slice = buf.as_mut_slice();
        for i in 0..BUFFER_SIZE {
            slice[i] = pattern_byte(BUFFER_SIZE - 1 - i);
        }
    }
    {
        let slice = buf.as_slice();
        for i in 0..BUFFER_SIZE {
            if slice[i] != pattern_byte(BUFFER_SIZE - 1 - i) {
                environment::log("FAIL: buffer unusable after child exit");
                return 1;
            }
        }
    }
    environment::log("Buffer transfer test: buffer still usable after child exit");

    environment::log("PASS");
    0
}
