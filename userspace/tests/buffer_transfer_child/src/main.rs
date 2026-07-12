#![no_std]
#![no_main]

use libpanda::{buffer, environment, ipc::Channel};

/// Must match the parent's `BUFFER_SIZE`.
const BUFFER_SIZE: usize = 4096;

fn pattern_byte(i: usize) -> u8 {
    (i % 256) as u8
}

/// Must match the parent's `CHILD_MARKER`.
const CHILD_MARKER: u8 = 0x77;

libpanda::main! {
    environment::log("Buffer transfer child: starting");

    let Some(parent) = Channel::parent() else {
        environment::log("FAIL: no parent channel");
        return 1;
    };

    let mut msg = [0u8; 64];
    let (len, attached) = match parent.recv_with_handle(&mut msg) {
        Ok(result) => result,
        Err(_) => {
            environment::log("FAIL: recv_with_handle failed");
            return 1;
        }
    };
    if &msg[..len] != b"buffer attached" {
        environment::log("FAIL: unexpected message payload");
        return 1;
    }
    let Some(buffer_handle) = attached else {
        environment::log("FAIL: expected a transferred buffer handle");
        return 1;
    };
    environment::log("Buffer transfer child: received transferred buffer handle");

    // Negative case: OP_BUFFER_MAP on a non-buffer handle must fail with
    // InvalidHandle rather than succeeding or crashing. The parent channel
    // handle (this process's own end of the spawn channel) is a handy
    // non-buffer handle to use here.
    if buffer::map(parent.untyped_handle()).is_ok() {
        environment::log("FAIL: mapping a non-buffer handle should have failed");
        return 1;
    }
    environment::log("Buffer transfer child: non-buffer handle correctly rejected");

    // Map the buffer into this process. This is the operation under test:
    // the handle alone (received via handle transfer) isn't usable until
    // this call installs a mapping in THIS process's address space.
    let Ok(vaddr1) = buffer::map(buffer_handle) else {
        environment::log("FAIL: OP_BUFFER_MAP failed");
        return 1;
    };
    environment::log("Buffer transfer child: mapped buffer");

    // Idempotence policy check: mapping the same buffer again must yield a
    // second, independent mapping rather than the same address back (see
    // `SharedBuffer::map_into_process`'s doc comment for the rationale).
    let Ok(vaddr2) = buffer::map(buffer_handle) else {
        environment::log("FAIL: second OP_BUFFER_MAP failed");
        return 1;
    };
    if vaddr1 == vaddr2 {
        environment::log("FAIL: mapping the same buffer twice returned the same address");
        return 1;
    }
    environment::log("Buffer transfer child: double-map produced an independent mapping");

    // SAFETY: `vaddr1` was just returned by a successful OP_BUFFER_MAP for
    // a BUFFER_SIZE-byte buffer, so this process has a valid BUFFER_SIZE-byte
    // mapping there.
    let slice = unsafe { core::slice::from_raw_parts(vaddr1 as *const u8, BUFFER_SIZE) };
    for i in 0..BUFFER_SIZE {
        if slice[i] != pattern_byte(i) {
            environment::log("FAIL: pattern mismatch — not sharing physical memory with parent");
            return 1;
        }
    }
    environment::log("Buffer transfer child: verified parent's pattern");

    // Reply pattern: overwrite the buffer through THIS mapping. The parent
    // must observe this through its own, separate mapping once we signal.
    // SAFETY: same mapping as above, now taken mutably.
    let slice = unsafe { core::slice::from_raw_parts_mut(vaddr1 as *mut u8, BUFFER_SIZE) };
    slice.fill(CHILD_MARKER);
    environment::log("Buffer transfer child: wrote reply pattern");

    if parent.send(b"child done").is_err() {
        environment::log("FAIL: signalling parent failed");
        return 1;
    }
    environment::log("Buffer transfer child: signalled parent");

    0
}
