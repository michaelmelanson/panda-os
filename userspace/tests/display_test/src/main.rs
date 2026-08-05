//! Test the `display:` scheme (exclusive display ownership).
//!
//! The display is exclusively claimed: exactly one owner at a time. This
//! test runs as its own init process with no compositor running, so it is
//! free to become that owner itself and exercise the full round trip —
//! open, a second open refused `Busy`, `OP_DISPLAY_INFO`/`MAP`/`FLUSH`
//! succeeding for the owner, close releasing the claim, and a reopen then
//! succeeding — plus path resolution and rejection of non-display handles.
//!
//! Before Phase 5 of plans/userspace-compositor.md this round trip was not
//! testable: the in-kernel compositor held the display's claim permanently,
//! so every userspace open was refused `Busy` and that was the only outcome
//! this test could assert (see the Phase 2 note in the plan). This is that
//! debt being paid off.

#![no_std]
#![no_main]

use libpanda::{ErrorCode, Handle, environment, file, sys};
use panda_abi::{SurfaceInfoOut, SurfaceRect};

libpanda::main! {
    environment::log("display_test: starting");

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

    // A path that resolves to no display device is NotFound, not Busy.
    match environment::open("display:/pci/display/7", 0, 0) {
        Err(ErrorCode::NotFound) => {
            environment::log("display_test: nonexistent display index rejected with NotFound")
        }
        _ => {
            environment::log("FAIL: nonexistent display index did not report NotFound");
            return 1;
        }
    }

    // A path resolving to a real device that is not the display is also
    // NotFound: the display scheme only opens the display.
    match environment::open("display:/pci/storage/0", 0, 0) {
        Err(ErrorCode::NotFound) => {
            environment::log("display_test: non-display device rejected with NotFound")
        }
        _ => {
            environment::log("FAIL: non-display device was accepted by the display scheme");
            return 1;
        }
    }

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

    environment::log("PASS");
    0
}
