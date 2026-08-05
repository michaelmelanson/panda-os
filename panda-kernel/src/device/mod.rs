//! Device registry for the userspace driver model.
//!
//! Tracks every device the kernel has enumerated (across all bus types),
//! who — if anyone — currently owns (has claimed) it, and drives the
//! subscription/replay mechanism that notifies driver processes of
//! arrivals and removals. See `plans/device-driver-model.md`.
//!
//! IOMMU-independent by design: this module only tracks claim ownership and
//! posts events. Actual hardware access (MMIO mapping, DMA, IRQ routing) is
//! Phase 6, gated on IOMMU support.

pub mod pci;
pub mod subscription;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spinning_top::Spinlock;

use panda_abi::device::{
    AcpiPath, BusType, DeviceIdentity, IoPortAddress, PciAddress, UsbAddress,
};

use crate::process::ProcessId;
use crate::resource::MailboxRef;

pub use subscription::SubscriptionRegistry;

/// Kernel-internal device identifier. Stable for the device's lifetime
/// (until removed); distinct from the per-subscriber claim token.
pub type DeviceId = u64;

/// Bus-specific device information: enough both to build a `DeviceIdentity`
/// for `DeviceEvent` and to match against a subscriber's raw match bytes.
#[derive(Debug, Clone, Copy)]
pub enum DeviceInfo {
    Pci {
        address: PciAddress,
        vendor_id: u16,
        device_id: u16,
        class: u32,
    },
    Usb {
        address: UsbAddress,
        vendor_id: u16,
        product_id: u16,
        class: u8,
        subclass: u8,
        protocol: u8,
    },
    Acpi {
        path: AcpiPath,
        hid: [u8; 8],
    },
    IoPort {
        address: IoPortAddress,
    },
}

impl DeviceInfo {
    pub fn bus_type(&self) -> BusType {
        match self {
            DeviceInfo::Pci { .. } => BusType::Pci,
            DeviceInfo::Usb { .. } => BusType::Usb,
            DeviceInfo::Acpi { .. } => BusType::Acpi,
            DeviceInfo::IoPort { .. } => BusType::IoPort,
        }
    }

    /// The per-bus identity to embed in a `DeviceEvent`.
    pub fn identity(&self) -> DeviceIdentity {
        match self {
            DeviceInfo::Pci { address, .. } => DeviceIdentity { pci: *address },
            DeviceInfo::Usb { address, .. } => DeviceIdentity { usb: *address },
            DeviceInfo::Acpi { path, .. } => DeviceIdentity { acpi: *path },
            DeviceInfo::IoPort { address } => DeviceIdentity { ioport: *address },
        }
    }

    /// Whether the raw bytes of a bus-specific `*DeviceId` match struct
    /// (as supplied to `OP_DEVICE_SUBSCRIBE`) match this device. Returns
    /// `false` (rather than panicking) if `match_bytes` isn't sized for this
    /// bus's match struct — malformed input from userspace simply matches
    /// nothing.
    pub fn matches(&self, match_bytes: &[u8]) -> bool {
        match self {
            DeviceInfo::Pci {
                vendor_id,
                device_id,
                class,
                ..
            } => match read_pod::<panda_abi::device::PciDeviceId>(match_bytes) {
                Some(id) => id.matches(*vendor_id, *device_id, *class),
                None => false,
            },
            DeviceInfo::Usb {
                vendor_id,
                product_id,
                class,
                subclass,
                protocol,
                ..
            } => match read_pod::<panda_abi::device::UsbDeviceId>(match_bytes) {
                Some(id) => usb_matches(&id, *vendor_id, *product_id, *class, *subclass, *protocol),
                None => false,
            },
            DeviceInfo::Acpi { hid, .. } => {
                match read_pod::<panda_abi::device::AcpiDeviceId>(match_bytes) {
                    Some(id) => id.hid == *hid,
                    None => false,
                }
            }
            DeviceInfo::IoPort { address } => {
                match read_pod::<panda_abi::device::IoPortDeviceId>(match_bytes) {
                    Some(id) => id.base == address.base && id.size == address.size,
                    None => false,
                }
            }
        }
    }
}

/// Reinterpret `bytes` as a `#[repr(C)]` POD value `T`, if `bytes` is
/// exactly `size_of::<T>()` long. Returns `None` (never panics/UB) on a
/// length mismatch — callers pass attacker-controlled bytes from a syscall.
fn read_pod<T: Copy>(bytes: &[u8]) -> Option<T> {
    if bytes.len() != core::mem::size_of::<T>() {
        return None;
    }
    // Safety: `T` is a `#[repr(C)]` struct of plain integer/byte fields
    // (see panda_abi::device), so any bit pattern of the right length is a
    // valid value of `T`. `read_unaligned` is used because `bytes` is not
    // guaranteed to satisfy `T`'s alignment.
    Some(unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const T) })
}

fn usb_matches(
    id: &panda_abi::device::UsbDeviceId,
    vendor_id: u16,
    product_id: u16,
    class: u8,
    subclass: u8,
    protocol: u8,
) -> bool {
    use panda_abi::device::{
        USB_MATCH_CLASS, USB_MATCH_PRODUCT, USB_MATCH_PROTOCOL, USB_MATCH_SUBCLASS,
        USB_MATCH_VENDOR,
    };
    let flags = id.match_flags;
    if flags & USB_MATCH_VENDOR != 0 && id.vendor_id != 0xFFFF && id.vendor_id != vendor_id {
        return false;
    }
    if flags & USB_MATCH_PRODUCT != 0 && id.product_id != 0xFFFF && id.product_id != product_id {
        return false;
    }
    if flags & USB_MATCH_CLASS != 0 && id.device_class != class {
        return false;
    }
    if flags & USB_MATCH_SUBCLASS != 0 && id.device_subclass != subclass {
        return false;
    }
    if flags & USB_MATCH_PROTOCOL != 0 && id.device_protocol != protocol {
        return false;
    }
    true
}

/// Errors returned by [`DeviceRegistry::claim`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimError {
    /// The token is unknown, already used, or not owned by the calling
    /// process.
    InvalidToken,
    /// The token was valid, but another process already owns this device.
    AlreadyClaimed,
}

/// Tracks known devices, their claim ownership, and drives the subscription
/// registry. See the module doc comment.
pub struct DeviceRegistry {
    devices: BTreeMap<DeviceId, DeviceInfo>,
    owners: BTreeMap<DeviceId, ProcessId>,
    next_device_id: DeviceId,
    /// Pending, unconsumed claim tokens: token -> (device, subscriber pid).
    /// A token is inserted when minted (subscribe replay / post_added) and
    /// removed the first time `claim` is called with it, whether or not
    /// that claim succeeds — this is what makes it single-use.
    tokens: BTreeMap<u64, (DeviceId, ProcessId)>,
    next_token: u64,
    subscriptions: SubscriptionRegistry,
}

impl DeviceRegistry {
    pub const fn new() -> Self {
        Self {
            devices: BTreeMap::new(),
            owners: BTreeMap::new(),
            next_device_id: 0,
            tokens: BTreeMap::new(),
            next_token: 1, // 0 is reserved to mean "no token" (EVENT_DEVICE_REMOVED)
            subscriptions: SubscriptionRegistry::new(),
        }
    }

    fn mint_token(
        tokens: &mut BTreeMap<u64, (DeviceId, ProcessId)>,
        next_token: &mut u64,
        device: DeviceId,
        pid: ProcessId,
    ) -> u64 {
        let token = *next_token;
        *next_token += 1;
        tokens.insert(token, (device, pid));
        token
    }

    /// Register a newly-enumerated device, notifying any subscribers that
    /// already match it. Called by bus enumeration code (`device::pci`).
    pub fn register(&mut self, info: DeviceInfo) -> DeviceId {
        let id = self.next_device_id;
        self.next_device_id += 1;
        self.devices.insert(id, info);

        let tokens = &mut self.tokens;
        let next_token = &mut self.next_token;
        let mut mint = |device, pid| Self::mint_token(tokens, next_token, device, pid);
        let posted = self.subscriptions.post_added(id, &info, &mut mint);
        log::info!(
            "device: registered {:?} as device {} ({} subscriber(s) notified)",
            info,
            id,
            posted.len()
        );
        id
    }

    /// Subscribe `owner_pid` to `bus_type` devices matching `match_bytes`,
    /// replaying `EVENT_DEVICE_ADDED` (with a fresh token each) for every
    /// currently-known match. Returns the replayed `(DeviceId, token)`
    /// pairs.
    pub fn subscribe(
        &mut self,
        bus_type: BusType,
        match_bytes: Vec<u8>,
        owner_pid: ProcessId,
        mailbox: MailboxRef,
    ) -> Vec<(DeviceId, u64)> {
        let tokens = &mut self.tokens;
        let next_token = &mut self.next_token;
        let mut mint = |device, pid| Self::mint_token(tokens, next_token, device, pid);
        let replayed =
            self.subscriptions
                .subscribe(bus_type, match_bytes, owner_pid, mailbox, &self.devices, &mut mint);
        log::info!(
            "device: process {:?} subscribed to {:?} ({} existing match(es) replayed)",
            owner_pid,
            bus_type,
            replayed.len()
        );
        replayed
    }

    /// Claim a device using a token previously handed to `pid`. Consumes
    /// the token (single-use) regardless of whether the claim succeeds.
    pub fn claim(&mut self, token: u64, pid: ProcessId) -> Result<DeviceId, ClaimError> {
        let Some(&(device_id, owner_pid)) = self.tokens.get(&token) else {
            log::warn!("device: claim with unknown/already-used token {} by {:?}", token, pid);
            return Err(ClaimError::InvalidToken);
        };
        if owner_pid != pid {
            // Not this process's token: leave it alone (don't burn the
            // rightful owner's still-valid token) and fail.
            log::warn!(
                "device: process {:?} tried to claim token {} owned by {:?}",
                pid,
                token,
                owner_pid
            );
            return Err(ClaimError::InvalidToken);
        }
        self.tokens.remove(&token);

        if self.owners.contains_key(&device_id) {
            log::warn!("device: {:?} claim of device {} denied, already owned", pid, device_id);
            return Err(ClaimError::AlreadyClaimed);
        }
        self.owners.insert(device_id, pid);
        log::info!("device: {:?} claimed device {}", pid, device_id);
        Ok(device_id)
    }

    /// Release a device's claim (e.g. on device removal or owning-process
    /// exit), posting `EVENT_DEVICE_REMOVED` to all matching subscribers.
    pub fn release(&mut self, device_id: DeviceId) {
        let owner = self.owners.remove(&device_id);
        if let Some(info) = self.devices.get(&device_id) {
            self.subscriptions.post_removed(info);
        }
        log::info!("device: released device {} (was owned by {:?})", device_id, owner);
    }

    /// Release every device claimed by `pid`. Called on process exit.
    pub fn release_all_owned_by(&mut self, pid: ProcessId) {
        let owned: Vec<DeviceId> = self
            .owners
            .iter()
            .filter(|&(_, &owner)| owner == pid)
            .map(|(&id, _)| id)
            .collect();
        if !owned.is_empty() {
            log::info!("device: process {:?} exiting, releasing {} device(s)", pid, owned.len());
        }
        for id in owned {
            self.release(id);
        }
    }

    /// The current owner of a device, if claimed.
    pub fn owner(&self, device_id: DeviceId) -> Option<ProcessId> {
        self.owners.get(&device_id).copied()
    }
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// The global device registry.
pub static DEVICE_REGISTRY: Spinlock<DeviceRegistry> = Spinlock::new(DeviceRegistry::new());

/// Release every device held by `pid`. Called from the process-exit path
/// (`scheduler::remove_process`) so a crashed or exited driver's claims are
/// torn down and `EVENT_DEVICE_REMOVED` is posted to subscribers, without
/// the driver needing to explicitly release anything.
pub fn release_all_owned_by(pid: ProcessId) {
    DEVICE_REGISTRY.lock().release_all_owned_by(pid);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Mailbox;

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

    fn wildcard_pci_match_bytes() -> Vec<u8> {
        let id = panda_abi::device::PciDeviceId {
            vendor_id: 0xFFFF,
            device_id: 0xFFFF,
            class: 0,
            class_mask: 0,
        };
        let ptr = &id as *const _ as *const u8;
        unsafe { core::slice::from_raw_parts(ptr, core::mem::size_of_val(&id)) }.to_vec()
    }

    fn test_mailbox() -> (alloc::sync::Arc<Mailbox>, MailboxRef) {
        let mailbox = Mailbox::new();
        // Any fixed handle id works for these tests: MailboxRef only needs
        // it to tag posted events, and these tests only assert the token
        // returned by `subscribe`/`register`, not mailbox contents.
        let mailbox_ref = MailboxRef::new(&mailbox, 0x2000_0000_0000_0001);
        (mailbox, mailbox_ref)
    }

    #[test]
    fn subscribe_replays_added_with_valid_token() {
        let mut registry = DeviceRegistry::new();
        let pid = ProcessId::new();
        let device_id = registry.register(pci_info(0x1AF4, 0x1052));

        let (_mailbox, mailbox_ref) = test_mailbox();
        let replayed = registry.subscribe(BusType::Pci, wildcard_pci_match_bytes(), pid, mailbox_ref);

        assert_eq!(replayed.len(), 1);
        let (replayed_device, token) = replayed[0];
        assert_eq!(replayed_device, device_id);
        assert_ne!(token, 0);
    }

    #[test]
    fn token_is_single_use() {
        let mut registry = DeviceRegistry::new();
        let pid = ProcessId::new();
        registry.register(pci_info(0x1AF4, 0x1052));

        let (_mailbox, mailbox_ref) = test_mailbox();
        let replayed = registry.subscribe(BusType::Pci, wildcard_pci_match_bytes(), pid, mailbox_ref);
        let (_device, token) = replayed[0];

        assert!(registry.claim(token, pid).is_ok());
        assert_eq!(registry.claim(token, pid), Err(ClaimError::InvalidToken));
    }

    #[test]
    fn claim_by_non_holder_fails() {
        let mut registry = DeviceRegistry::new();
        let owner_pid = ProcessId::new();
        let attacker_pid = ProcessId::new();
        registry.register(pci_info(0x1AF4, 0x1052));

        let (_mailbox, mailbox_ref) = test_mailbox();
        let replayed = registry.subscribe(BusType::Pci, wildcard_pci_match_bytes(), owner_pid, mailbox_ref);
        let (_device, token) = replayed[0];

        assert_eq!(
            registry.claim(token, attacker_pid),
            Err(ClaimError::InvalidToken)
        );
        // The rightful owner's token must still be valid afterwards.
        assert!(registry.claim(token, owner_pid).is_ok());
    }

    #[test]
    fn wildcard_matches_any_vendor() {
        let mut registry = DeviceRegistry::new();
        let pid = ProcessId::new();
        registry.register(pci_info(0x8086, 0x1234));

        let (_mailbox, mailbox_ref) = test_mailbox();
        let replayed = registry.subscribe(BusType::Pci, wildcard_pci_match_bytes(), pid, mailbox_ref);
        assert_eq!(replayed.len(), 1);
    }

    #[test]
    fn exclusive_claim_second_process_gets_already_claimed() {
        let mut registry = DeviceRegistry::new();
        let pid_a = ProcessId::new();
        let pid_b = ProcessId::new();
        registry.register(pci_info(0x1AF4, 0x1052));

        let (_mailbox_a, mailbox_ref_a) = test_mailbox();
        let (_mailbox_b, mailbox_ref_b) = test_mailbox();

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

    #[test]
    fn process_exit_releases_claim_and_posts_removed() {
        let mut registry = DeviceRegistry::new();
        let pid = ProcessId::new();
        let device_id = registry.register(pci_info(0x1AF4, 0x1052));

        let (_mailbox, mailbox_ref) = test_mailbox();
        let replayed = registry.subscribe(BusType::Pci, wildcard_pci_match_bytes(), pid, mailbox_ref);
        let (_device, token) = replayed[0];
        registry.claim(token, pid).unwrap();
        assert_eq!(registry.owner(device_id), Some(pid));

        registry.release_all_owned_by(pid);
        assert_eq!(registry.owner(device_id), None);

        // Resources are fully freed: a fresh subscribe + claim succeeds again.
        let (_mailbox2, mailbox_ref2) = test_mailbox();
        let replayed2 = registry.subscribe(BusType::Pci, wildcard_pci_match_bytes(), pid, mailbox_ref2);
        let (_device2, token2) = replayed2[0];
        assert!(registry.claim(token2, pid).is_ok());
    }
}
