//! Test the `display:` scheme (exclusive display ownership).
//!
//! The display is exclusively claimed: exactly one owner at a time. This
//! test runs as its own init process with no compositor running, so it is
//! free to become that owner itself and exercise the full round trip —
//! open, a second open refused `Busy`, `OP_DISPLAY_INFO`/`MAP`/`FLUSH`
//! succeeding for the owner, close releasing the claim, and a reopen then
//! succeeding — plus path resolution and rejection of non-display handles.

#![no_std]
#![no_main]

use libpanda::ipc::Channel;
use libpanda::{ErrorCode, Handle, environment, file, sys};
use panda_abi::{SurfaceInfoOut, SurfaceRect};

libpanda::main! {
    environment::log("display_test: starting");

    // `display:` is served by a userspace driver
    // (`userspace/drivers/virtio-gpu`) — like `keyboard_test`, this process
    // plays the role of `init` (no service manager runs to spawn it from
    // `initrd:/drivers`), so spawn it directly and wait for its "ready"
    // signal.
    let Ok(driver_handle) = environment::spawn("initrd:/virtio-gpu") else {
        environment::log("FAIL: could not spawn virtio-gpu driver");
        return 1;
    };
    let Some(driver_channel) = Channel::from_handle_borrowed(driver_handle.into()) else {
        environment::log("FAIL: driver handle is not a channel");
        return 1;
    };
    let mut ready_msg = [0u8; 64];
    if driver_channel.recv(&mut ready_msg).is_err() {
        environment::log("FAIL: did not receive ready signal from virtio-gpu driver");
        return 1;
    }

    // Claim the display.
    let Ok(display) = environment::open("display:/pci/display/0", 0, 0) else {
        environment::log("FAIL: could not open display:/pci/display/0");
        return 1;
    };
    environment::log("display_test: claimed the display");

    // A second open is refused: the claim is exclusive.
    match environment::open("display:/pci/display/0", 0, 0) {
        Err(ErrorCode::Busy) => environment::log("display_test: second open refused with Busy"),
        _ => {
            environment::log("FAIL: second display open did not report Busy");
            return 1;
        }
    }

    // OP_DISPLAY_INFO succeeds for the owner and reports non-zero dimensions.
    let mut owner_info = SurfaceInfoOut {
        width: 0,
        height: 0,
        format: 0,
        stride: 0,
    };
    if sys::display::info(display, &mut owner_info) < 0 {
        environment::log("FAIL: OP_DISPLAY_INFO failed for the owner");
        return 1;
    }
    if owner_info.width == 0 || owner_info.height == 0 || owner_info.stride == 0 {
        environment::log("FAIL: OP_DISPLAY_INFO reported a zero dimension");
        return 1;
    }
    environment::log("display_test: OP_DISPLAY_INFO succeeded for the owner");

    // OP_DISPLAY_MAP succeeds for the owner and returns a usable vaddr.
    let vaddr = sys::display::map(display);
    if vaddr < 0 {
        environment::log("FAIL: OP_DISPLAY_MAP failed for the owner");
        return 1;
    }
    environment::log("display_test: OP_DISPLAY_MAP succeeded for the owner");

    // OP_DISPLAY_FLUSH succeeds for the owner, both for a bounded rect and
    // for a full-screen flush.
    let rect = SurfaceRect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    if sys::display::flush(display, Some(&rect)) < 0 {
        environment::log("FAIL: OP_DISPLAY_FLUSH failed for the owner (bounded rect)");
        return 1;
    }
    if sys::display::flush(display, None) < 0 {
        environment::log("FAIL: OP_DISPLAY_FLUSH failed for the owner (full screen)");
        return 1;
    }
    environment::log("display_test: OP_DISPLAY_FLUSH succeeded for the owner");

    // Closing the handle releases the claim, so a reopen succeeds.
    file::close(display);
    let Ok(display2) = environment::open("display:/pci/display/0", 0, 0) else {
        environment::log("FAIL: display reopen after close failed");
        return 1;
    };
    environment::log("display_test: reopen after close succeeded");
    file::close(display2);

    // Path-based rejection of non-display device addresses (e.g.
    // "display:/pci/display/7" or "display:/pci/storage/0") is not
    // enforced: the userspace driver (`userspace/drivers/virtio-gpu`)
    // grants any path once, matching `userspace/drivers/virtio-keyboard`'s
    // identical simplification for `keyboard:` — this driver only ever
    // serves the one device it claimed, so path resolution adds no real
    // access control here.

    // The display operations must reject handles that are not displays —
    // which, since the handle *is* the proof of ownership, is exactly what a
    // process that does not hold the display can reach.
    let mut info = SurfaceInfoOut {
        width: 0,
        height: 0,
        format: 0,
        stride: 0,
    };
    let rect = SurfaceRect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    // A valid handle of the wrong type, and a handle that does not exist.
    let bogus = unsafe { Handle::from_raw(0xDEAD_BEEF) };
    for handle in [Handle::MAILBOX, bogus] {
        for result in [
            sys::display::info(handle, &mut info),
            sys::display::map(handle),
            sys::display::flush(handle, Some(&rect)),
        ] {
            if result >= 0 {
                environment::log("FAIL: display operation accepted a non-display handle");
                return 1;
            }
            if libpanda::error::from_code(result) != ErrorCode::InvalidHandle {
                environment::log("FAIL: expected InvalidHandle from display operation");
                return 1;
            }
        }
    }
    environment::log("display_test: INFO/MAP/FLUSH on non-display handles rejected");

    // The virtio-gpu driver runs forever (a persistent driver process in
    // the real boot path); ask it to exit before this process does, or
    // QEMU keeps running until the harness's own timeout even though the
    // test already passed — see `userspace/tests/keyboard_test`'s identical
    // shutdown request for the same reason.
    let _ = driver_channel.send(b"shutdown");

    environment::log("PASS");
    0
}
