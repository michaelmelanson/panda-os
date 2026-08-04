//! Helper process for claim_test: opens a block device raw and exits without
//! closing it, so the parent can verify that process exit alone (dropping the
//! child's handle table) releases the device claim.
//!
//! This used to claim the framebuffer via `surface:/fb0`. It can't any more:
//! the in-kernel compositor now holds the display's claim for as long as it
//! runs (see plans/userspace-compositor.md, Phase 2), so no userspace process
//! can take the display. The block device exercises the identical
//! `ClaimGuard` drop-on-exit path.

#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    environment::log("claim_child: opening block:/pci/storage/0");

    match environment::open("block:/pci/storage/0", 0, 0) {
        Ok(_handle) => {
            environment::log("claim_child: opened block device, exiting without closing it");
            0
        }
        Err(_) => {
            environment::log("claim_child: FAIL could not open block device");
            1
        }
    }
}
