#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use panda_abi::device::{BusType, PciAddress, PciDeviceId};
use panda_kernel::device::{ClaimError, DeviceInfo, DeviceRegistry};
use panda_kernel::process::ProcessId;
use panda_kernel::resource::{Mailbox, MailboxRef};

panda_kernel::test_harness!(
    subscribe_replays_added_with_valid_token,
    token_is_single_use,
    claim_by_non_holder_fails,
    wildcard_matching,
    exclusive_claim_second_process_gets_already_claimed,
    process_exit_releases_claim
);

fn pci_info(vendor: u16, device: u16, class: u32) -> DeviceInfo {
    DeviceInfo::Pci {
        address: PciAddress {
            segment: 0,
            bus: 0,
            device: 3,
            function: 0,
            _pad: [0; 3],
        },
        vendor_id: vendor,
        device_id: device,
        class,
    }
}

fn pci_match_bytes(vendor: u16, device: u16, class: u32, class_mask: u32) -> Vec<u8> {
    let id = PciDeviceId {
        vendor_id: vendor,
        device_id: device,
        class,
        class_mask,
    };
    let ptr = &id as *const _ as *const u8;
    unsafe { core::slice::from_raw_parts(ptr, core::mem::size_of_val(&id)) }.to_vec()
}

fn wildcard_pci_match_bytes() -> Vec<u8> {
    pci_match_bytes(0xFFFF, 0xFFFF, 0, 0)
}

fn test_mailbox(tag: u64) -> (alloc::sync::Arc<Mailbox>, MailboxRef) {
    let mailbox = Mailbox::new();
    let mailbox_ref = MailboxRef::new(&mailbox, 0x2000_0000_0000_0000 | tag);
    (mailbox, mailbox_ref)
}

/// Register a device, subscribe after the fact, and confirm the replay
/// immediately hands back a valid claim token for it.
fn subscribe_replays_added_with_valid_token() {
    let mut registry = DeviceRegistry::new();
    let pid = ProcessId::new();
    let device_id = registry.register(pci_info(0x1AF4, 0x1052, 0));

    let (_mailbox, mailbox_ref) = test_mailbox(1);
    let replayed = registry.subscribe(BusType::Pci, wildcard_pci_match_bytes(), pid, mailbox_ref);

    assert_eq!(replayed.len(), 1);
    let (replayed_device, token) = replayed[0];
    assert_eq!(replayed_device, device_id);
    assert_ne!(token, 0);
}

/// A token can claim its device exactly once; a second attempt with the
/// same token is rejected even though the first attempt succeeded.
fn token_is_single_use() {
    let mut registry = DeviceRegistry::new();
    let pid = ProcessId::new();
    registry.register(pci_info(0x1AF4, 0x1052, 0));

    let (_mailbox, mailbox_ref) = test_mailbox(2);
    let replayed = registry.subscribe(BusType::Pci, wildcard_pci_match_bytes(), pid, mailbox_ref);
    let (_device, token) = replayed[0];

    assert!(registry.claim(token, pid).is_ok());
    assert_eq!(registry.claim(token, pid), Err(ClaimError::InvalidToken));
}

/// A process that never held a token cannot use it to claim a device, and
/// doing so does not burn the token for its rightful owner.
fn claim_by_non_holder_fails() {
    let mut registry = DeviceRegistry::new();
    let owner_pid = ProcessId::new();
    let attacker_pid = ProcessId::new();
    registry.register(pci_info(0x1AF4, 0x1052, 0));

    let (_mailbox, mailbox_ref) = test_mailbox(3);
    let replayed = registry.subscribe(
        BusType::Pci,
        wildcard_pci_match_bytes(),
        owner_pid,
        mailbox_ref,
    );
    let (_device, token) = replayed[0];

    assert_eq!(
        registry.claim(token, attacker_pid),
        Err(ClaimError::InvalidToken)
    );
    assert!(registry.claim(token, owner_pid).is_ok());
}

/// `vendor_id == 0xFFFF` matches any vendor, `class_mask == 0` ignores
/// class, and combining both matches everything.
fn wildcard_matching() {
    let mut registry = DeviceRegistry::new();
    let pid = ProcessId::new();
    registry.register(pci_info(0x8086, 0x1234, 0x0C_03_00));

    // vendor_id wildcard only, exact device id, no class check.
    let (_mailbox_a, mailbox_ref_a) = test_mailbox(4);
    let replayed_a = registry.subscribe(
        BusType::Pci,
        pci_match_bytes(0xFFFF, 0x1234, 0, 0),
        pid,
        mailbox_ref_a,
    );
    assert_eq!(replayed_a.len(), 1);

    // class_mask 0 ignores a mismatched class.
    let (_mailbox_b, mailbox_ref_b) = test_mailbox(5);
    let replayed_b = registry.subscribe(
        BusType::Pci,
        pci_match_bytes(0x8086, 0x1234, 0xFFFFFF, 0),
        pid,
        mailbox_ref_b,
    );
    assert_eq!(replayed_b.len(), 1);

    // Fully wildcarded matches everything.
    let (_mailbox_c, mailbox_ref_c) = test_mailbox(6);
    let replayed_c = registry.subscribe(BusType::Pci, wildcard_pci_match_bytes(), pid, mailbox_ref_c);
    assert_eq!(replayed_c.len(), 1);

    // A non-matching device id is excluded.
    let (_mailbox_d, mailbox_ref_d) = test_mailbox(7);
    let replayed_d = registry.subscribe(
        BusType::Pci,
        pci_match_bytes(0x8086, 0x9999, 0, 0),
        pid,
        mailbox_ref_d,
    );
    assert_eq!(replayed_d.len(), 0);
}

/// Two processes subscribe to the same device and each get their own valid
/// token; the first claim wins and the second gets AlreadyClaimed even
/// though its token, checked independently, is valid.
fn exclusive_claim_second_process_gets_already_claimed() {
    let mut registry = DeviceRegistry::new();
    let pid_a = ProcessId::new();
    let pid_b = ProcessId::new();
    registry.register(pci_info(0x1AF4, 0x1052, 0));

    let (_mailbox_a, mailbox_ref_a) = test_mailbox(8);
    let (_mailbox_b, mailbox_ref_b) = test_mailbox(9);

    let replayed_a = registry.subscribe(BusType::Pci, wildcard_pci_match_bytes(), pid_a, mailbox_ref_a);
    let replayed_b = registry.subscribe(BusType::Pci, wildcard_pci_match_bytes(), pid_b, mailbox_ref_b);

    let (_device_a, token_a) = replayed_a[0];
    let (_device_b, token_b) = replayed_b[0];
    assert_ne!(token_a, token_b);

    assert!(registry.claim(token_a, pid_a).is_ok());
    assert_eq!(
        registry.claim(token_b, pid_b),
        Err(ClaimError::AlreadyClaimed)
    );
}

/// When a process that claimed a device exits (without explicitly
/// releasing), the kernel releases the claim; a fresh subscribe + claim for
/// the same device then succeeds, proving resources were fully freed.
fn process_exit_releases_claim() {
    let mut registry = DeviceRegistry::new();
    let pid = ProcessId::new();
    let device_id = registry.register(pci_info(0x1AF4, 0x1052, 0));

    let (_mailbox, mailbox_ref) = test_mailbox(10);
    let replayed = registry.subscribe(BusType::Pci, wildcard_pci_match_bytes(), pid, mailbox_ref);
    let (_device, token) = replayed[0];
    registry.claim(token, pid).unwrap();
    assert_eq!(registry.owner(device_id), Some(pid));

    // Simulate process exit.
    registry.release_all_owned_by(pid);
    assert_eq!(registry.owner(device_id), None);

    let (_mailbox2, mailbox_ref2) = test_mailbox(11);
    let replayed2 = registry.subscribe(BusType::Pci, wildcard_pci_match_bytes(), pid, mailbox_ref2);
    let (_device2, token2) = replayed2[0];
    assert!(registry.claim(token2, pid).is_ok());
}
