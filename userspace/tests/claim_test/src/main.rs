//! Test exclusive device ownership (the claim table).
//!
//! Covers the claim guard's whole lifecycle on a block device — second open
//! fails `Busy`, close releases, process exit releases (via claim_child) —
//! plus the two claims that are held by something other than a plain raw
//! open: the in-kernel compositor's claim on the display, and ext2's claim on
//! a mounted block device.
//!
//! The lifecycle cases used to run against `surface:/fb0`. They can't any
//! more: since Phase 2 of plans/userspace-compositor.md the in-kernel
//! compositor claims the display for as long as it runs, so no userspace
//! process can acquire it. The block device exercises exactly the same
//! `ClaimGuard` code paths, and the display's now-permanent claim is asserted
//! directly below instead.

#![no_std]
#![no_main]

use libpanda::{ErrorCode, environment, file, process};

libpanda::main! {
    environment::log("claim_test: starting");

    // Test 1: a raw block open is exclusive — a second open fails Busy
    let Ok(dev) = environment::open("block:/pci/storage/0", 0, 0) else {
        environment::log("FAIL: could not open block:/pci/storage/0");
        return 1;
    };
    match environment::open("block:/pci/storage/0", 0, 0) {
        Err(ErrorCode::Busy) => {
            environment::log("claim_test: second block open refused with Busy")
        }
        Err(_) => {
            environment::log("FAIL: second block open failed with wrong error");
            return 1;
        }
        Ok(_) => {
            environment::log("FAIL: second block open succeeded");
            return 1;
        }
    }

    // Test 2: closing the handle releases the claim
    file::close(dev);
    let Ok(dev2) = environment::open("block:/pci/storage/0", 0, 0) else {
        environment::log("FAIL: reopen after close failed");
        return 1;
    };
    environment::log("claim_test: reopen after close succeeded");
    file::close(dev2);

    // Test 3: process exit releases the claim — the child opens the device
    // and exits without closing it
    let Ok(child) = environment::spawn("file:/initrd/claim_child") else {
        environment::log("FAIL: could not spawn claim_child");
        return 1;
    };
    if process::wait(child) != 0 {
        environment::log("FAIL: claim_child exited non-zero");
        return 1;
    }
    let Ok(dev3) = environment::open("block:/pci/storage/0", 0, 0) else {
        environment::log("FAIL: block open after child exit failed");
        return 1;
    };
    environment::log("claim_test: block open after child exit succeeded");
    file::close(dev3);

    // Test 4: the display is claimed by the in-kernel compositor, so both the
    // display scheme and the legacy raw framebuffer path are refused
    match environment::open("display:/pci/display/0", 0, 0) {
        Err(ErrorCode::Busy) => {
            environment::log("claim_test: display open refused with Busy")
        }
        _ => {
            environment::log("FAIL: display open was not refused with Busy");
            return 1;
        }
    }
    match environment::open("surface:/fb0", 0, 0) {
        Err(ErrorCode::Busy) => environment::log("claim_test: fb0 open refused with Busy"),
        _ => {
            environment::log("FAIL: fb0 open was not refused with Busy");
            return 1;
        }
    }

    // Test 5: a mounted block device refuses raw opens
    if environment::mount("ext2", "/mnt").is_err() {
        environment::log("FAIL: could not mount ext2");
        return 1;
    }
    match environment::open("block:/pci/storage/0", 0, 0) {
        Err(ErrorCode::Busy) => {
            environment::log("claim_test: raw open of mounted block device refused with Busy")
        }
        Err(_) => {
            environment::log("FAIL: raw block open failed with wrong error");
            return 1;
        }
        Ok(_) => {
            environment::log("FAIL: raw open of mounted block device succeeded");
            return 1;
        }
    }

    environment::log("PASS");
    0
}
