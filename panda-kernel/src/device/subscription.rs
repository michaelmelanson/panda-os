//! Subscription registry: tracks which processes want to hear about which
//! devices, and replays/posts `EVENT_DEVICE_ADDED` / `EVENT_DEVICE_REMOVED`.
//!
//! See `plans/device-driver-model.md` ("Subscription replay") and
//! `panda-kernel/src/device/mod.rs` (`DeviceRegistry`, which owns a
//! `SubscriptionRegistry` and mints device tokens as it replays/posts
//! events).

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use panda_abi::device::{BusType, EVENT_DEVICE_ADDED, EVENT_DEVICE_REMOVED};

use crate::process::ProcessId;
use crate::resource::MailboxRef;

use super::{DeviceId, DeviceInfo};

/// A single active subscription: a process listening for devices matching
/// `match_bytes` (the raw contents of a `*DeviceId` match struct) on
/// `bus_type`.
struct SubscriberEntry {
    bus_type: BusType,
    match_bytes: Vec<u8>,
    owner_pid: ProcessId,
    mailbox: MailboxRef,
}

impl SubscriberEntry {
    fn matches(&self, bus_type: BusType, info: &DeviceInfo) -> bool {
        self.bus_type == bus_type && info.matches(&self.match_bytes)
    }
}

/// Tracks all active device subscriptions and drives replay/post events.
///
/// `(BusType, match_bytes)` -> `Vec<MailboxRef>`, per the design doc — kept
/// here as a flat `Vec<SubscriberEntry>` since the number of concurrent
/// driver subscriptions is small (one per driver process) and a flat scan is
/// simpler than reimplementing wildcard-aware map lookups.
#[derive(Default)]
pub struct SubscriptionRegistry {
    subscribers: Vec<SubscriberEntry>,
}

impl SubscriptionRegistry {
    pub const fn new() -> Self {
        Self {
            subscribers: Vec::new(),
        }
    }

    /// Register a new subscription, then immediately replay
    /// `EVENT_DEVICE_ADDED` for every currently-known device that matches
    /// it. `mint_token` allocates a fresh single-use claim token for
    /// `(device, owner_pid)`; see `DeviceRegistry`.
    ///
    /// Returns the `(DeviceId, token)` pairs replayed, for callers that need
    /// them synchronously (e.g. tests, and the syscall handler until a full
    /// out-of-band event payload delivery path exists — see
    /// `panda-kernel/src/syscall/device.rs` for that gap).
    pub fn subscribe(
        &mut self,
        bus_type: BusType,
        match_bytes: Vec<u8>,
        owner_pid: ProcessId,
        mailbox: MailboxRef,
        devices: &BTreeMap<DeviceId, DeviceInfo>,
        mint_token: &mut impl FnMut(DeviceId, ProcessId) -> u64,
    ) -> Vec<(DeviceId, u64)> {
        let entry = SubscriberEntry {
            bus_type,
            match_bytes,
            owner_pid,
            mailbox,
        };

        let replayed = Self::replay_to_new_subscriber(&entry, devices, mint_token);
        self.subscribers.push(entry);
        replayed
    }

    /// Post `EVENT_DEVICE_ADDED` (with a fresh token) to `entry` for every
    /// device in `devices` it matches. Called once, at subscribe time, for
    /// devices that already existed before the subscription was created —
    /// this is what eliminates the start-order race described in the design
    /// doc.
    fn replay_to_new_subscriber(
        entry: &SubscriberEntry,
        devices: &BTreeMap<DeviceId, DeviceInfo>,
        mint_token: &mut impl FnMut(DeviceId, ProcessId) -> u64,
    ) -> Vec<(DeviceId, u64)> {
        let mut replayed = Vec::new();
        for (&id, info) in devices {
            if entry.matches(info.bus_type(), info) {
                let token = mint_token(id, entry.owner_pid);
                entry.mailbox.post_event(EVENT_DEVICE_ADDED);
                replayed.push((id, token));
            }
        }
        replayed
    }

    /// Post `EVENT_DEVICE_ADDED` to every subscriber matching the newly
    /// added device, minting each of them a fresh token.
    pub fn post_added(
        &self,
        id: DeviceId,
        info: &DeviceInfo,
        mint_token: &mut impl FnMut(DeviceId, ProcessId) -> u64,
    ) -> Vec<(ProcessId, u64)> {
        let mut posted = Vec::new();
        for entry in &self.subscribers {
            if entry.matches(info.bus_type(), info) {
                let token = mint_token(id, entry.owner_pid);
                entry.mailbox.post_event(EVENT_DEVICE_ADDED);
                posted.push((entry.owner_pid, token));
            }
        }
        posted
    }

    /// Post `EVENT_DEVICE_REMOVED` to every subscriber matching the removed
    /// device — regardless of whether that subscriber previously received
    /// `EVENT_DEVICE_ADDED` for it (e.g. it may have subscribed after the
    /// device was already claimed by someone else).
    pub fn post_removed(&self, info: &DeviceInfo) {
        for entry in &self.subscribers {
            if entry.matches(info.bus_type(), info) {
                entry.mailbox.post_event(EVENT_DEVICE_REMOVED);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DeviceInfo;
    use panda_abi::device::PciAddress;

    fn pci_info(vendor: u16, device: u16) -> DeviceInfo {
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
            class: 0,
        }
    }

    fn pci_match_bytes(vendor: u16, device: u16) -> Vec<u8> {
        let id = panda_abi::device::PciDeviceId {
            vendor_id: vendor,
            device_id: device,
            class: 0,
            class_mask: 0,
        };
        let ptr = &id as *const _ as *const u8;
        unsafe { core::slice::from_raw_parts(ptr, core::mem::size_of_val(&id)) }.to_vec()
    }

    #[test]
    fn wildcard_vendor_matches_any() {
        let info = pci_info(0x1AF4, 0x1052);
        assert!(info.matches(&pci_match_bytes(0xFFFF, 0x1052)));
        assert!(!info.matches(&pci_match_bytes(0xFFFF, 0x9999)));
    }
}
