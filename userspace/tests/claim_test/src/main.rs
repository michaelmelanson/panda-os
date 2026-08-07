//! Test exclusive device ownership (the claim table).
//!
//! Covers the claim guard's whole lifecycle on a block device — second open
//! fails `Busy`, close releases, process exit releases (via claim_child) —
//! plus ext2's claim on a mounted block device, and the same full lifecycle
//! against the real `display:` scheme.
//!
//! The display lifecycle case used to be untestable: before Phase 5 of
//! plans/userspace-compositor.md the in-kernel compositor held the display's
//! claim permanently, so every open from a test process failed `Busy` and
//! only that one outcome could be asserted (see the Phase 2 note in the
//! plan). With the in-kernel compositor deleted, nothing holds the display
//! at boot, so this test now exercises the full open/Busy/close/reopen cycle
//! against `display:/pci/display/0` directly — the "carried forward as
//! explicit, tracked debt against Phase 5" round trip the plan called for.

#![no_std]
#![no_main]

use libpanda::ipc::Channel;
use libpanda::{ErrorCode, environment, file, process};

libpanda::main! {
    environment::log("claim_test: starting");

    // This test plays the role of `init` (it's the initrd's `init`
    // binary), so unlike the real boot path there is no service manager
    // running to spawn either driver — do it directly, the same way
    // `keyboard_test`/`display_test` spawn their drivers (M4 Phase 2 and
    // roadmap M4, plans/device-driver-model.md). Both `block:` opens below
    // plus the ext2 mount in Test 5 need virtio-blk running, and Test 4
    // needs virtio-gpu running.
    let Ok(blk_driver_handle) = environment::spawn("file:/initrd/virtio-blk") else {
        environment::log("FAIL: could not spawn virtio-blk driver");
        return 1;
    };
    let Some(blk_driver_channel) = Channel::from_handle_borrowed(blk_driver_handle.into()) else {
        environment::log("FAIL: virtio-blk driver handle is not a channel");
        return 1;
    };
    let mut ready_msg = [0u8; 64];
    if blk_driver_channel.recv(&mut ready_msg).is_err() {
        environment::log("FAIL: did not receive ready signal from virtio-blk driver");
        return 1;
    }

    let Ok(gpu_driver_handle) = environment::spawn("file:/initrd/virtio-gpu") else {
        environment::log("FAIL: could not spawn virtio-gpu driver");
        return 1;
    };
    let Some(gpu_driver_channel) = Channel::from_handle_borrowed(gpu_driver_handle.into()) else {
        environment::log("FAIL: virtio-gpu driver handle is not a channel");
        return 1;
    };
    let mut ready_msg = [0u8; 64];
    if gpu_driver_channel.recv(&mut ready_msg).is_err() {
        environment::log("FAIL: did not receive ready signal from virtio-gpu driver");
        return 1;
    }

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

    // Test 4: the display claim's full lifecycle — open, second open Busy,
    // close releases, reopen succeeds.
    let Ok(display) = environment::open("display:/pci/display/0", 0, 0) else {
        environment::log("FAIL: could not open display:/pci/display/0");
        return 1;
    };
    match environment::open("display:/pci/display/0", 0, 0) {
        Err(ErrorCode::Busy) => {
            environment::log("claim_test: second display open refused with Busy")
        }
        Err(_) => {
            environment::log("FAIL: second display open failed with wrong error");
            return 1;
        }
        Ok(_) => {
            environment::log("FAIL: second display open succeeded");
            return 1;
        }
    }
    file::close(display);
    let Ok(display2) = environment::open("display:/pci/display/0", 0, 0) else {
        environment::log("FAIL: display reopen after close failed");
        return 1;
    };
    environment::log("claim_test: display reopen after close succeeded");
    file::close(display2);

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

    // Both drivers run forever (persistent driver processes in the real
    // boot path), so this test (playing `init`) must ask each to exit
    // before returning itself, or QEMU never shuts down — see
    // `block_test`'s/`display_test`'s identical shutdown requests.
    let _ = blk_driver_channel.send(b"shutdown");
    let _ = gpu_driver_channel.send(b"shutdown");

    environment::log("PASS");
    0
}
