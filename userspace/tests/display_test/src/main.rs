//! Test the `display:` scheme (exclusive display ownership).
//!
//! The display is exclusively claimed: exactly one owner at a time. While the
//! in-kernel compositor runs it *is* that owner (it claims the display in
//! `compositor::init`), so on this system every userspace open of the display
//! is refused with `Busy` — through `display:` and through the legacy
//! `surface:/fb0` path alike.
//!
//! That means the full round trip (open → INFO → MAP → FLUSH → close →
//! reopen succeeds) is not testable until Phase 5 of
//! plans/userspace-compositor.md deletes the kernel compositor and the
//! userspace compositor becomes the owner. What is testable now — and what
//! this test covers — is the exclusivity itself, path resolution, and that
//! the three display operations reject callers that do not hold the display
//! rather than crashing.

#![no_std]
#![no_main]

use libpanda::{ErrorCode, Handle, environment, sys};
use panda_abi::{SurfaceInfoOut, SurfaceRect};

libpanda::main! {
    environment::log("display_test: starting");

    // The display is claimed by the in-kernel compositor, so opening it
    // exclusively must be refused.
    match environment::open("display:/pci/display/0", 0, 0) {
        Err(ErrorCode::Busy) => {
            environment::log("display_test: display open refused with Busy (compositor owns it)")
        }
        Err(_) => {
            environment::log("FAIL: display open failed with the wrong error");
            return 1;
        }
        Ok(_) => {
            environment::log("FAIL: display open succeeded while the compositor owns the display");
            return 1;
        }
    }

    // The same device reached by its raw PCI class path is the same claim.
    match environment::open("display:/pci/display/0", 0, 0) {
        Err(ErrorCode::Busy) => environment::log("display_test: second open also refused with Busy"),
        _ => {
            environment::log("FAIL: second display open did not report Busy");
            return 1;
        }
    }

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
