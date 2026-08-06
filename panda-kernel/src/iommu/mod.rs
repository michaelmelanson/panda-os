//! Kernel integration for the `iommu` crate: hardware discovery, the global
//! IOMMU instance, and the passthrough domain all virtio devices are
//! assigned to today.

mod dmar_acpi;
pub mod hal;
pub mod manager;

use alloc::boxed::Box;
use alloc::vec::Vec;

use iommu::vtd::dmar::DrhEntry;
use iommu::vtd::{VtdIommu, VtdUnit};
use iommu::{DomainId, Iommu, PciAddress};
use log::{info, warn};
use spinning_top::RwSpinlock;

use hal::{KernelFrameAllocator, KernelMmioAccess};

/// Size, in bytes, of the memory-mapped register bank for one VT-d unit.
/// QEMU's `intel-iommu` device (and real hardware in practice) exposes its
/// register set within a single 4 KB page.
const VTD_REGISTER_BANK_SIZE: usize = 0x1000;

/// Fallback physical range identity-mapped in the passthrough domain if the
/// kernel heap's physical range is unavailable for some reason (risk #4):
/// conservatively covers every address `DmaBuffer` can return without
/// needing to track the UEFI memory map past boot.
const PASSTHROUGH_FALLBACK_LIMIT: u64 = 4 * 1024 * 1024 * 1024;

/// The physical range to identity-map in the passthrough domain: the kernel
/// heap's actual physical range, rounded up to the next 2 MB boundary. Every
/// physical address `DmaBuffer`/`allocate_physical` can return falls within
/// the kernel heap (see `memory::allocate_physical`), so this tightly (and
/// correctly) bounds risk #4's identity map without the cost of mapping a
/// full 4 GB span at 4 KB granularity when the heap is much smaller.
fn passthrough_limit() -> u64 {
    let (base, size) = crate::memory::heap_phys_range();
    if size == 0 {
        return PASSTHROUGH_FALLBACK_LIMIT;
    }
    let end = base + size;
    (end + 0x1F_FFFF) & !0x1F_FFFF
}

static IOMMU: RwSpinlock<Option<Box<dyn Iommu>>> = RwSpinlock::new(None);
static PASSTHROUGH_DOMAIN: RwSpinlock<Option<DomainId>> = RwSpinlock::new(None);

/// Discover VT-d hardware via the DMAR ACPI table, bring up translation, and
/// assign every currently-enumerated PCI device to a single identity-mapped
/// passthrough domain. Falls back to logging if only an IVRS (AMD-Vi) table
/// is found, since AMD-Vi is a stub. If neither table is present, the kernel
/// runs without an IOMMU exactly as it did before this feature existed.
pub fn init() {
    let dmar = match dmar_acpi::read_dmar() {
        Ok(dmar) => dmar,
        Err(dmar_acpi::DmarReadError::NotPresent) => {
            warn!("iommu: no DMAR table found; AMD-Vi detected but not yet supported, or no IOMMU present");
            return;
        }
        Err(dmar_acpi::DmarReadError::Parse(err)) => {
            warn!("iommu: failed to parse DMAR table: {:?}", err);
            return;
        }
    };

    if dmar.drhd.is_empty() {
        warn!("iommu: DMAR table present but contains no DRHD entries");
        return;
    }

    info!(
        "iommu: found {} DRHD unit(s), {} RMRR range(s)",
        dmar.drhd.len(),
        dmar.rmrr.len()
    );

    let frame_allocator = KernelFrameAllocator::new();
    let mut units: Vec<VtdUnit<KernelFrameAllocator, KernelMmioAccess>> = Vec::new();

    for drhd in &dmar.drhd {
        match build_unit(drhd, frame_allocator.clone()) {
            Ok(unit) => units.push(unit),
            Err(err) => {
                warn!(
                    "iommu: failed to initialise DRHD at {:#x}: {:?}",
                    drhd.register_base, err
                );
            }
        }
    }

    if units.is_empty() {
        warn!("iommu: no DRHD units could be initialised; IOMMU disabled");
        return;
    }

    let limit = passthrough_limit();
    let vtd = VtdIommu::new(units, dmar.rmrr.clone(), frame_allocator, limit);

    if let Err(err) = vtd.activate() {
        warn!("iommu: failed to activate translation: {:?}", err);
        return;
    }

    let passthrough = match vtd.init_passthrough_domain(limit) {
        Ok(domain) => domain,
        Err(err) => {
            warn!("iommu: failed to create passthrough domain: {:?}", err);
            return;
        }
    };

    let devices = enumerate_pci_addresses();
    if let Err(err) = vtd.assign_all_known_devices(passthrough, &devices) {
        warn!(
            "iommu: failed to assign one or more devices to the passthrough domain: {:?}",
            err
        );
    }
    for device in &devices {
        manager::assign(*device, passthrough);
    }

    info!(
        "iommu: VT-d active, passthrough domain {} covers 0..{:#x}, {} device(s) assigned",
        passthrough,
        limit,
        devices.len()
    );

    *PASSTHROUGH_DOMAIN.write() = Some(passthrough);
    *IOMMU.write() = Some(Box::new(vtd));
}

fn build_unit(
    drhd: &DrhEntry,
    frame_allocator: KernelFrameAllocator,
) -> Result<VtdUnit<KernelFrameAllocator, KernelMmioAccess>, iommu::IommuError> {
    let mmio = KernelMmioAccess::new(drhd.register_base, VTD_REGISTER_BANK_SIZE);
    VtdUnit::new(drhd, mmio, frame_allocator)
}

/// Enumerate every currently-known PCI device as a `PciAddress`.
fn enumerate_pci_addresses() -> Vec<PciAddress> {
    let devices = core::cell::RefCell::new(Vec::new());
    crate::pci::enumerate_pci_devices(|device| {
        let addr = device.address();
        devices
            .borrow_mut()
            .push(PciAddress::new(addr.segment, addr.bus, addr.slot, addr.function));
    });
    devices.into_inner()
}

/// The active IOMMU implementation, if hardware was found and initialised.
pub fn get() -> Option<&'static dyn Iommu> {
    // Safety: once set, `IOMMU` is never cleared or replaced, so a reference
    // derived from the read guard's contents is valid for 'static. We use a
    // raw pointer to detach the lifetime from the guard.
    let guard = IOMMU.read();
    guard.as_ref().map(|b| {
        let ptr: *const dyn Iommu = b.as_ref();
        unsafe { &*ptr }
    })
}

/// The domain ID all existing (kernel-driven) devices are assigned to.
pub fn passthrough_domain() -> Option<DomainId> {
    *PASSTHROUGH_DOMAIN.read()
}
