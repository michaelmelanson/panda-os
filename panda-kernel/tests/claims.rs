//! Integration tests for the device claim table (`devices::claims`).

#![no_std]
#![no_main]

use panda_kernel::device_address::DeviceAddress;
use panda_kernel::devices::claims::{ClaimError, ClaimOwner, claim};

panda_kernel::test_harness!(
    second_claim_fails_while_held,
    drop_releases_claim_for_reclaim,
    independent_addresses_claim_independently,
    guard_reports_address_and_owner,
);

fn addr(device: u8) -> DeviceAddress {
    DeviceAddress::Pci {
        bus: 0,
        device,
        function: 0,
    }
}

fn second_claim_fails_while_held() {
    let address = addr(0x10);

    let guard = claim(address.clone(), ClaimOwner::RawOpen).expect("first claim should succeed");

    let second = claim(address.clone(), ClaimOwner::Mount);
    assert_eq!(
        second.unwrap_err(),
        ClaimError::Busy,
        "claiming an already-held device should fail with Busy"
    );

    // The first claim is still held; a third attempt should also fail.
    let third = claim(address, ClaimOwner::Display);
    assert_eq!(third.unwrap_err(), ClaimError::Busy);

    drop(guard);
}

fn drop_releases_claim_for_reclaim() {
    let address = addr(0x11);

    let guard = claim(address.clone(), ClaimOwner::RawOpen).expect("first claim should succeed");
    drop(guard);

    let reclaimed = claim(address, ClaimOwner::Mount);
    assert!(
        reclaimed.is_ok(),
        "dropping a claim guard should release the claim for reuse"
    );
}

fn independent_addresses_claim_independently() {
    let address_a = addr(0x12);
    let address_b = addr(0x13);

    let guard_a =
        claim(address_a.clone(), ClaimOwner::RawOpen).expect("claim on device A should succeed");
    let guard_b =
        claim(address_b.clone(), ClaimOwner::Display).expect("claim on device B should succeed");

    // Each device is independently busy for a second claimant...
    assert_eq!(
        claim(address_a.clone(), ClaimOwner::Mount).unwrap_err(),
        ClaimError::Busy
    );
    assert_eq!(
        claim(address_b.clone(), ClaimOwner::Mount).unwrap_err(),
        ClaimError::Busy
    );

    // ...and releasing one does not affect the other.
    drop(guard_a);
    assert!(claim(address_a, ClaimOwner::Mount).is_ok());
    assert_eq!(
        claim(address_b, ClaimOwner::Mount).unwrap_err(),
        ClaimError::Busy
    );

    drop(guard_b);
}

fn guard_reports_address_and_owner() {
    let address = addr(0x14);
    let guard = claim(address.clone(), ClaimOwner::Display).expect("claim should succeed");

    assert_eq!(*guard.address(), address);
    assert_eq!(guard.owner(), ClaimOwner::Display);
}
