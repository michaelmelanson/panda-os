//! Helper process for claim_test: opens the framebuffer surface and exits
//! without closing it, so the parent can verify that process exit alone
//! (dropping the child's handle table) releases the device claim.

#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    environment::log("claim_child: opening surface:/fb0");

    match environment::open("surface:/fb0", 0, 0) {
        Ok(_handle) => {
            environment::log("claim_child: opened fb0, exiting without closing it");
            0
        }
        Err(_) => {
            environment::log("claim_child: FAIL could not open fb0");
            1
        }
    }
}
