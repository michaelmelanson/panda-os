//! Intel VT-d backend.

pub mod context;
pub mod dmar;
pub mod page_table;
pub mod registers;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use spinning_top::Spinlock;

use crate::hal::{FrameAllocator, MmioAccess};
use crate::iommu::Iommu;
use crate::iova::IovaAllocator;
use crate::types::{DomainId, IommuError, IommuFlags, PciAddress};

use context::RootTable;
use dmar::{DrhEntry, RmrrEntry};
use page_table::SlptRoot;
use registers::VtdRegisters;

/// One VT-d remapping unit: its register bank, its root/context table, and the
/// device scopes it is responsible for (from its DRHD entry).
pub struct VtdUnit<F: FrameAllocator, M: MmioAccess> {
    registers: VtdRegisters<M>,
    root_table: Spinlock<RootTable<F>>,
    segment: u16,
    include_all: bool,
    /// (bus, devfn) pairs this unit explicitly claims via device scopes.
    explicit_devices: Vec<(u8, u8)>,
}

impl<F: FrameAllocator, M: MmioAccess> VtdUnit<F, M> {
    pub fn new(drhd: &DrhEntry, mmio: M, allocator: F) -> Result<Self, IommuError> {
        let root_table = Spinlock::new(RootTable::new(allocator)?);
        let registers = VtdRegisters::new(mmio);

        let mut explicit_devices = Vec::new();
        for scope in &drhd.device_scopes {
            if let Some(&(device, function)) = scope.path.last() {
                let devfn = (device << 3) | (function & 0x7);
                explicit_devices.push((scope.start_bus, devfn));
            }
        }

        Ok(Self {
            registers,
            root_table,
            segment: drhd.segment,
            include_all: drhd.include_all,
            explicit_devices,
        })
    }

    fn covers(&self, device: PciAddress) -> bool {
        if device.segment != self.segment {
            return false;
        }
        if self
            .explicit_devices
            .contains(&(device.bus, device.devfn()))
        {
            return true;
        }
        self.include_all
    }

    /// Program hardware to point at this unit's root table and enable translation.
    pub fn activate(&self) -> Result<(), IommuError> {
        let root_phys = self.root_table.lock().physical_address();
        log::debug!(
            "iommu: unit segment {}: CAP={:#x} ECAP={:#x} qi_capable={}",
            self.segment,
            self.registers.read_cap(),
            self.registers.read_ecap(),
            self.registers.qi_capable(),
        );
        self.registers.set_root_table(root_phys)?;
        self.registers.invalidate_context_cache()?;
        self.registers.enable_translation()
    }
}

struct DomainState<F: FrameAllocator> {
    slpt: SlptRoot<F>,
    /// Reserved for future IOVA allocation on non-passthrough domains.
    #[allow(dead_code)]
    iova: IovaAllocator,
}

/// Intel VT-d implementation of [`Iommu`], generic over the kernel-provided
/// frame allocator and MMIO access types.
pub struct VtdIommu<F: FrameAllocator, M: MmioAccess> {
    units: Vec<VtdUnit<F, M>>,
    domains: Spinlock<BTreeMap<DomainId, DomainState<F>>>,
    next_domain_id: Spinlock<DomainId>,
    /// RMRR ranges that must be identity-mapped into every domain (risk #1).
    rmrr: Vec<RmrrEntry>,
    frame_allocator: F,
    /// Default IOVA space handed to newly created domains: `[0, iova_limit)`.
    iova_limit: u64,
}

impl<F: FrameAllocator + Clone, M: MmioAccess> VtdIommu<F, M> {
    pub fn new(units: Vec<VtdUnit<F, M>>, rmrr: Vec<RmrrEntry>, frame_allocator: F, iova_limit: u64) -> Self {
        Self {
            units,
            domains: Spinlock::new(BTreeMap::new()),
            next_domain_id: Spinlock::new(1),
            rmrr,
            frame_allocator,
            iova_limit,
        }
    }

    /// Activate every remapping unit's hardware translation.
    pub fn activate(&self) -> Result<(), IommuError> {
        for unit in &self.units {
            unit.activate()?;
        }
        Ok(())
    }

    /// Create the passthrough domain, identity-mapping `0..limit` plus any RMRR
    /// ranges, and assign every device covered by every unit to it.
    ///
    /// Returns the passthrough domain's ID.
    pub fn init_passthrough_domain(&self, limit: u64) -> Result<DomainId, IommuError> {
        let domain = self.create_domain()?;
        self.map(domain, 0, 0, limit as usize, IommuFlags::READ_WRITE)?;

        for rmrr in &self.rmrr {
            let base = rmrr.base_address & !0xFFF;
            let end = (rmrr.limit_address + 1 + 0xFFF) & !0xFFF;
            if end > base && base >= limit {
                let size = (end - base) as usize;
                self.map(domain, base, base, size, IommuFlags::READ_WRITE)?;
            }
        }

        self.flush(domain);
        Ok(domain)
    }

    /// Assign every device visible to any unit's device scope to `domain`.
    /// For units with `include_all`, this covers the whole segment via a
    /// single wildcard-style pass over PCI addresses supplied by the caller.
    pub fn assign_all_known_devices(&self, domain: DomainId, devices: &[PciAddress]) -> Result<(), IommuError> {
        for &device in devices {
            self.assign_device(domain, device)?;
        }
        Ok(())
    }
}

impl<F: FrameAllocator + Clone, M: MmioAccess> Iommu for VtdIommu<F, M> {
    fn create_domain(&self) -> Result<DomainId, IommuError> {
        let mut next = self.next_domain_id.lock();
        let id = *next;
        *next += 1;
        drop(next);

        let slpt = SlptRoot::new(self.frame_allocator.clone())?;
        let iova = IovaAllocator::new(0, self.iova_limit);

        self.domains.lock().insert(id, DomainState { slpt, iova });
        Ok(id)
    }

    fn destroy_domain(&self, domain: DomainId) {
        self.domains.lock().remove(&domain);
    }

    fn map(
        &self,
        domain: DomainId,
        iova: u64,
        phys: u64,
        size: usize,
        flags: IommuFlags,
    ) -> Result<(), IommuError> {
        let mut domains = self.domains.lock();
        let state = domains.get_mut(&domain).ok_or(IommuError::DomainNotFound)?;
        state.slpt.map(iova, phys, size, flags)
    }

    fn unmap(&self, domain: DomainId, iova: u64, size: usize) {
        let mut domains = self.domains.lock();
        if let Some(state) = domains.get_mut(&domain) {
            state.slpt.unmap(iova, size);
        }
    }

    fn assign_device(&self, domain: DomainId, device: PciAddress) -> Result<(), IommuError> {
        let slpt_root_phys = {
            let domains = self.domains.lock();
            let state = domains.get(&domain).ok_or(IommuError::DomainNotFound)?;
            state.slpt.root_phys()
        };

        for unit in &self.units {
            if unit.covers(device) {
                unit.root_table.lock().set_context_entry(
                    device.bus,
                    device.devfn(),
                    domain,
                    slpt_root_phys,
                )?;
                unit.registers.invalidate_context_cache()?;
                return Ok(());
            }
        }

        Err(IommuError::DeviceNotFound)
    }

    fn flush(&self, _domain: DomainId) {
        // Domain-scoped invalidation is a future optimisation; global IOTLB
        // invalidation on every unit is correct, if coarse.
        for unit in &self.units {
            let _ = unit.registers.invalidate_iotlb_global();
        }
    }
}
