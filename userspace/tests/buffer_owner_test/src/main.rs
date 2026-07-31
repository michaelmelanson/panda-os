//! Test that buffer syscalls which depend on the allocating process's
//! mapping are safe when called on a *transferred* buffer handle.
//!
//! OP_FILE_READ_BUFFER / OP_FILE_WRITE_BUFFER dereference the allocator's
//! virtual address, and OP_BUFFER_FREE reclaims it — both are only
//! meaningful in the allocating process. Called from a process that merely
//! received the handle (M1.1 handle transfer), they must fail cleanly (or,
//! for free, drop only the handle) instead of dereferencing or reclaiming a
//! foreign address in the caller's address space.
//!
//! The parent allocates a buffer and transfers it; all the interesting
//! assertions happen in buffer_owner_child.

#![no_std]
#![no_main]

use libpanda::{buffer::Buffer, environment, ipc::Channel, process};

const BUFFER_SIZE: usize = 4096;
const PARENT_MARKER: u8 = 0x50; // 'P'

libpanda::main! {
    environment::log("Buffer owner test: starting");

    let Some(mut buf) = Buffer::alloc(BUFFER_SIZE) else {
        environment::log("FAIL: buffer alloc failed");
        return 1;
    };
    buf.as_mut_slice().fill(PARENT_MARKER);

    let Ok(child) = environment::spawn("file:/initrd/buffer_owner_child") else {
        environment::log("FAIL: spawn failed");
        return 1;
    };
    let Some(channel) = Channel::from_handle_borrowed(child.into()) else {
        environment::log("FAIL: child handle is not a channel");
        return 1;
    };

    if channel
        .send_with_handle(b"buffer attached", buf.handle())
        .is_err()
    {
        environment::log("FAIL: send_with_handle failed");
        return 1;
    }
    environment::log("Buffer owner test: transferred buffer to child");

    let exit_code = process::wait(child);
    if exit_code != 0 {
        environment::log("FAIL: child exited non-zero");
        return 1;
    }

    // The child never legitimately wrote to the buffer, so the parent's
    // marker must be untouched regardless of what the child attempted.
    if buf.as_slice().iter().any(|&b| b != PARENT_MARKER) {
        environment::log("FAIL: parent buffer was modified by child's failed operations");
        return 1;
    }
    environment::log("Buffer owner test: parent buffer untouched");

    environment::log("PASS");
    0
}
