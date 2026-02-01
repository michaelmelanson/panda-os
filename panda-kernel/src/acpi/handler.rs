use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use log::trace;
use spinning_top::Spinlock;
use x86_64::instructions::port::Port;
use x86_64::PhysAddr;

use crate::memory::PhysicalMapping;

/// Tracks ACPI physical mappings by their virtual address.
/// When unmap_physical_region is called, we look up and drop the mapping.
static ACPI_MAPPINGS: Spinlock<BTreeMap<usize, PhysicalMapping>> = Spinlock::new(BTreeMap::new());

/// Next mutex handle to allocate.
static NEXT_MUTEX_HANDLE: AtomicU64 = AtomicU64::new(1);

/// Tracks ACPI mutexes. Each handle maps to an atomic lock flag.
static ACPI_MUTEXES: Spinlock<BTreeMap<u64, &'static AtomicBool>> =
    Spinlock::new(BTreeMap::new());

#[derive(Clone, Copy)]
pub struct AcpiHandler;

/// Read a value of type `T` from a physical address using a temporary RAII mapping.
///
/// Creates a `PhysicalMapping`, reads the value via volatile access, and drops
/// the mapping automatically when done.
fn phys_read<T: Copy>(address: usize) -> T {
    let phys = PhysAddr::new(address as u64);
    let mapping = PhysicalMapping::new(phys, core::mem::size_of::<T>());
    mapping.read::<T>(0)
}

/// Write a value of type `T` to a physical address using a temporary RAII mapping.
///
/// Creates a `PhysicalMapping`, writes the value via volatile access, and drops
/// the mapping automatically when done.
fn phys_write<T: Copy>(address: usize, value: T) {
    let phys = PhysAddr::new(address as u64);
    let mapping = PhysicalMapping::new(phys, core::mem::size_of::<T>());
    mapping.write::<T>(0, value);
}

/// Calculate the ECAM virtual address for a PCI config space access.
///
/// Looks up the PCI segment group containing the given bus and returns
/// the virtual address for the device's config space at the given offset.
fn pci_config_addr(address: &acpi::PciAddress, offset: u16) -> Option<*mut u8> {
    let groups = crate::pci::PCI_SEGMENT_GROUPS.read();
    for group in groups.iter() {
        if group.group_id == address.segment()
            && address.bus() >= group.bus_number_start
            && address.bus() <= group.bus_number_end
        {
            let device_offset = ((address.bus() as u64 * 256)
                + (address.device() as u64 * 8)
                + address.function() as u64)
                * 4096;
            let addr =
                group.base_address.as_u64() + device_offset + offset as u64;
            return Some(addr as *mut u8);
        }
    }
    None
}

impl ::acpi::Handler for AcpiHandler {
    unsafe fn map_physical_region<T>(
        &self,
        physical_address: usize,
        size: usize,
    ) -> acpi::PhysicalMapping<Self, T> {
        let physical_start = PhysAddr::new(physical_address as u64);

        // Create a PhysicalMapping and store it for later cleanup
        let mapping = PhysicalMapping::new(physical_start, size);
        let virt_addr = mapping.virt_addr().as_u64() as usize;
        let virtual_start = unsafe { NonNull::new_unchecked(virt_addr as *mut _) };

        // Store the mapping so it can be dropped in unmap_physical_region
        ACPI_MAPPINGS.lock().insert(virt_addr, mapping);

        acpi::PhysicalMapping {
            physical_start: physical_start.as_u64() as usize,
            virtual_start,
            region_length: size,
            mapped_length: size,
            handler: *self,
        }
    }

    fn unmap_physical_region<T>(region: &acpi::PhysicalMapping<Self, T>) {
        // Remove and drop the mapping
        let virt_addr = region.virtual_start.as_ptr() as usize;
        ACPI_MAPPINGS.lock().remove(&virt_addr);
    }

    // --- Physical memory read/write ---
    //
    // These use temporary RAII PhysicalMapping wrappers. The mapping is created,
    // the volatile read/write is performed, and the mapping is dropped.

    fn read_u8(&self, address: usize) -> u8 {
        trace!("ACPI: read_u8 at {:#x}", address);
        phys_read::<u8>(address)
    }

    fn read_u16(&self, address: usize) -> u16 {
        trace!("ACPI: read_u16 at {:#x}", address);
        phys_read::<u16>(address)
    }

    fn read_u32(&self, address: usize) -> u32 {
        trace!("ACPI: read_u32 at {:#x}", address);
        phys_read::<u32>(address)
    }

    fn read_u64(&self, address: usize) -> u64 {
        trace!("ACPI: read_u64 at {:#x}", address);
        phys_read::<u64>(address)
    }

    fn write_u8(&self, address: usize, value: u8) {
        trace!("ACPI: write_u8 at {:#x} = {:#x}", address, value);
        phys_write::<u8>(address, value);
    }

    fn write_u16(&self, address: usize, value: u16) {
        trace!("ACPI: write_u16 at {:#x} = {:#x}", address, value);
        phys_write::<u16>(address, value);
    }

    fn write_u32(&self, address: usize, value: u32) {
        trace!("ACPI: write_u32 at {:#x} = {:#x}", address, value);
        phys_write::<u32>(address, value);
    }

    fn write_u64(&self, address: usize, value: u64) {
        trace!("ACPI: write_u64 at {:#x} = {:#x}", address, value);
        phys_write::<u64>(address, value);
    }

    // --- IO port read/write ---
    //
    // Uses x86 in/out instructions via the x86_64 crate's Port abstraction.

    fn read_io_u8(&self, port: u16) -> u8 {
        trace!("ACPI: read_io_u8 port {:#x}", port);
        let mut p = Port::<u8>::new(port);
        unsafe { p.read() }
    }

    fn read_io_u16(&self, port: u16) -> u16 {
        trace!("ACPI: read_io_u16 port {:#x}", port);
        let mut p = Port::<u16>::new(port);
        unsafe { p.read() }
    }

    fn read_io_u32(&self, port: u16) -> u32 {
        trace!("ACPI: read_io_u32 port {:#x}", port);
        let mut p = Port::<u32>::new(port);
        unsafe { p.read() }
    }

    fn write_io_u8(&self, port: u16, value: u8) {
        trace!("ACPI: write_io_u8 port {:#x} = {:#x}", port, value);
        let mut p = Port::<u8>::new(port);
        unsafe { p.write(value) }
    }

    fn write_io_u16(&self, port: u16, value: u16) {
        trace!("ACPI: write_io_u16 port {:#x} = {:#x}", port, value);
        let mut p = Port::<u16>::new(port);
        unsafe { p.write(value) }
    }

    fn write_io_u32(&self, port: u16, value: u32) {
        trace!("ACPI: write_io_u32 port {:#x} = {:#x}", port, value);
        let mut p = Port::<u32>::new(port);
        unsafe { p.write(value) }
    }

    // --- PCI configuration space read/write ---
    //
    // Uses the existing ECAM (memory-mapped) PCI config space infrastructure.
    // The ECAM regions are already mapped during PCI initialisation and persist
    // for the kernel lifetime.

    fn read_pci_u8(&self, address: acpi::PciAddress, offset: u16) -> u8 {
        trace!("ACPI: read_pci_u8 {:?} offset {:#x}", address, offset);
        let ptr = pci_config_addr(&address, offset)
            .expect("ACPI PCI read: segment/bus not found in ECAM mappings");
        unsafe { core::ptr::read_volatile(ptr) }
    }

    fn read_pci_u16(&self, address: acpi::PciAddress, offset: u16) -> u16 {
        trace!("ACPI: read_pci_u16 {:?} offset {:#x}", address, offset);
        let ptr = pci_config_addr(&address, offset)
            .expect("ACPI PCI read: segment/bus not found in ECAM mappings")
            as *const u16;
        unsafe { core::ptr::read_volatile(ptr) }
    }

    fn read_pci_u32(&self, address: acpi::PciAddress, offset: u16) -> u32 {
        trace!("ACPI: read_pci_u32 {:?} offset {:#x}", address, offset);
        let ptr = pci_config_addr(&address, offset)
            .expect("ACPI PCI read: segment/bus not found in ECAM mappings")
            as *const u32;
        unsafe { core::ptr::read_volatile(ptr) }
    }

    fn write_pci_u8(&self, address: acpi::PciAddress, offset: u16, value: u8) {
        trace!(
            "ACPI: write_pci_u8 {:?} offset {:#x} = {:#x}",
            address, offset, value
        );
        let ptr = pci_config_addr(&address, offset)
            .expect("ACPI PCI write: segment/bus not found in ECAM mappings");
        unsafe { core::ptr::write_volatile(ptr, value) }
    }

    fn write_pci_u16(&self, address: acpi::PciAddress, offset: u16, value: u16) {
        trace!(
            "ACPI: write_pci_u16 {:?} offset {:#x} = {:#x}",
            address, offset, value
        );
        let ptr = pci_config_addr(&address, offset)
            .expect("ACPI PCI write: segment/bus not found in ECAM mappings")
            as *mut u16;
        unsafe { core::ptr::write_volatile(ptr, value) }
    }

    fn write_pci_u32(&self, address: acpi::PciAddress, offset: u16, value: u32) {
        trace!(
            "ACPI: write_pci_u32 {:?} offset {:#x} = {:#x}",
            address, offset, value
        );
        let ptr = pci_config_addr(&address, offset)
            .expect("ACPI PCI write: segment/bus not found in ECAM mappings")
            as *mut u32;
        unsafe { core::ptr::write_volatile(ptr, value) }
    }

    // --- Timing operations ---

    fn nanos_since_boot(&self) -> u64 {
        crate::time::uptime_ns()
    }

    fn stall(&self, microseconds: u64) {
        // Busy-wait using the nanosecond-resolution TSC-based timer.
        let start_ns = crate::time::uptime_ns();
        let wait_ns = microseconds * 1_000;
        while crate::time::uptime_ns() < start_ns + wait_ns {
            core::hint::spin_loop();
        }
    }

    fn sleep(&self, milliseconds: u64) {
        // In a kernel context without a scheduler-aware sleep primitive,
        // fall back to busy-waiting with nanosecond resolution.
        let start_ns = crate::time::uptime_ns();
        let wait_ns = milliseconds * 1_000_000;
        while crate::time::uptime_ns() < start_ns + wait_ns {
            core::hint::spin_loop();
        }
    }

    // --- Mutex operations ---
    //
    // Simple spinlock-based mutex implementation using atomic handles.
    // Sufficient for ACPI AML evaluation which runs in kernel context.

    fn create_mutex(&self) -> acpi::Handle {
        let handle = NEXT_MUTEX_HANDLE.fetch_add(1, Ordering::Relaxed);
        let lock = Box::leak(Box::new(AtomicBool::new(false)));
        ACPI_MUTEXES.lock().insert(handle, lock);
        acpi::Handle(handle as u32)
    }

    fn acquire(&self, mutex: acpi::Handle, timeout: u16) -> Result<(), acpi::aml::AmlError> {
        let handle = mutex.0 as u64;
        let lock = {
            let mutexes = ACPI_MUTEXES.lock();
            match mutexes.get(&handle) {
                Some(lock) => *lock,
                None => return Err(acpi::aml::AmlError::InvalidName(None)),
            }
        };

        let start_ns = crate::time::uptime_ns();
        let timeout_ns = if timeout == 0xFFFF {
            u64::MAX
        } else {
            timeout as u64 * 1_000_000
        };

        loop {
            // Atomic compare-and-swap: try to transition false -> true
            if lock
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return Ok(());
            }

            if crate::time::uptime_ns().wrapping_sub(start_ns) >= timeout_ns {
                return Err(acpi::aml::AmlError::MutexAcquireTimeout);
            }
            core::hint::spin_loop();
        }
    }

    fn release(&self, mutex: acpi::Handle) {
        let handle = mutex.0 as u64;
        let mutexes = ACPI_MUTEXES.lock();
        if let Some(lock) = mutexes.get(&handle) {
            lock.store(false, Ordering::Release);
        }
    }
}
