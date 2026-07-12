//! Test exclusive device ownership (the claim table).
//!
//! Covers the two devices the claim table protects:
//! - the display framebuffer (`surface:/fb0`): second open fails Busy,
//!   close releases, and process exit releases (via claim_child);
//! - block devices: a device mounted by ext2 refuses raw `block:` opens.

#![no_std]
#![no_main]

use libpanda::{ErrorCode, environment, file, process};

libpanda::main! {
    environment::log("claim_test: starting");

    // Test 1: fb0 is exclusive — second open fails Busy
    let Ok(fb) = environment::open("surface:/fb0", 0, 0) else {
        environment::log("FAIL: could not open surface:/fb0");
        return 1;
    };
    match environment::open("surface:/fb0", 0, 0) {
        Err(ErrorCode::Busy) => environment::log("claim_test: second fb0 open refused with Busy"),
        Err(_) => {
            environment::log("FAIL: second fb0 open failed with wrong error");
            return 1;
        }
        Ok(_) => {
            environment::log("FAIL: second fb0 open succeeded");
            return 1;
        }
    }

    // Test 2: closing the handle releases the claim
    file::close(fb);
    let Ok(fb2) = environment::open("surface:/fb0", 0, 0) else {
        environment::log("FAIL: reopen after close failed");
        return 1;
    };
    environment::log("claim_test: reopen after close succeeded");
    file::close(fb2);

    // Test 3: process exit releases the claim — the child opens fb0 and
    // exits without closing it
    let Ok(child) = environment::spawn("file:/initrd/claim_child") else {
        environment::log("FAIL: could not spawn claim_child");
        return 1;
    };
    if process::wait(child) != 0 {
        environment::log("FAIL: claim_child exited non-zero");
        return 1;
    }
    let Ok(fb3) = environment::open("surface:/fb0", 0, 0) else {
        environment::log("FAIL: fb0 open after child exit failed");
        return 1;
    };
    environment::log("claim_test: fb0 open after child exit succeeded");
    file::close(fb3);

    // Test 4: a mounted block device refuses raw opens
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
