//! Helper process for claim_test: opens a block device raw and exits without
//! closing it, so the parent can verify that process exit alone (dropping the
//! child's handle table) releases the device claim.
//!
//! Uses the block device rather than the display because the display driver
//! enforces exclusivity itself, whereas the block device exercises the
//! `ClaimGuard` drop-on-exit path directly.

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
