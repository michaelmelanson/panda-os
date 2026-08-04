//! Test that the legacy raw framebuffer path is exclusively owned.
//!
//! This test used to draw a four-quadrant test pattern straight into the
//! framebuffer through `surface:/fb0` and verify it with a screenshot. That
//! is no longer possible, deliberately: since Phase 2 of
//! plans/userspace-compositor.md the in-kernel compositor holds the display's
//! exclusive claim for as long as it runs, so `surface:/fb0` — which claims
//! the same device — is always `Busy`.
//!
//! Handing `/fb0` out while the compositor is writing the same pixels was
//! precisely the unsynchronized-aliasing hazard the claim table exists to
//! prevent, so the new behaviour is the fix, not a regression. End-to-end
//! "pixels reach the screen" coverage lives in the compositor screenshot
//! tests (`window_test`, `alpha_test`, `multi_window_test`,
//! `partial_refresh_test`, `window_move_test`), which go through
//! `surface:/window`. `/fb0` itself is retired outright in Phase 4/5, when
//! this test is replaced by `compositor_protocol_test`.

#![no_std]
#![no_main]

use libpanda::{ErrorCode, environment};

libpanda::main! {
    environment::log("Surface test starting");

    match environment::open("surface:/fb0", 0, 0) {
        Err(ErrorCode::Busy) => {
            environment::log("PASS: fb0 refused with Busy (the compositor owns the display)")
        }
        Err(_) => {
            environment::log("FAIL: fb0 open failed with the wrong error");
            return 1;
        }
        Ok(_) => {
            environment::log("FAIL: fb0 open succeeded while the compositor owns the display");
            return 1;
        }
    }

    environment::log("PASS");
    0
}
