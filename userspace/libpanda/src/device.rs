//! Userspace device driver model: ELF device-match tables, `MmioRegion`,
//! and the `OP_DEVICE_*` syscall wrappers.
//!
//! See `plans/device-driver-model.md`. Of the six syscall wrappers below,
//! only [`device_subscribe`] and [`device_claim`] round-trip to a working
//! kernel handler (`panda-kernel/src/syscall/device.rs`). The other four —
//! [`device_map_mmio`], [`dma_alloc`], [`dma_free`], and
//! [`device_subscribe_irq`] — return `ErrorCode::NotSupported` without
//! issuing a syscall: their kernel-side implementations require IOMMU
//! support (a separate, later task), and there is no dispatch for their
//! `OP_*` codes yet. Returning early here (rather than dispatching to a
//! syscall that would just hit the kernel's default `NotSupported` arm)
//! keeps the failure obvious and avoids a partially-wired code path that
//! looks more complete than it is.

// `panda_abi::device::Handle` is just `u64`, distinct from (and not
// re-exported over) `crate::Handle`, libpanda's typed handle wrapper.
pub use panda_abi::device::{
    AcpiDeviceId, AcpiPath, BusType, DeviceEvent, DeviceIdentity, EVENT_DEVICE_ADDED,
    EVENT_DEVICE_IRQ, EVENT_DEVICE_REMOVED, IoPortAddress, IoPortDeviceId, OP_DEVICE_CLAIM,
    OP_DEVICE_MAP_MMIO, OP_DEVICE_SUBSCRIBE, OP_DEVICE_SUBSCRIBE_IRQ, OP_DMA_ALLOC, OP_DMA_FREE,
    PCI_MATCH_ANY, PciAddress, PciDeviceId, USB_MATCH_CLASS, USB_MATCH_PRODUCT,
    USB_MATCH_PROTOCOL, USB_MATCH_SUBCLASS, USB_MATCH_VENDOR, UsbAddress, UsbDeviceId,
};

use crate::Handle;
use crate::error::{self, Result};
use crate::sys;

/// Subscribe to device add/remove events for `bus_type`, matching the raw
/// bytes of `match_data` (a bus-specific `*DeviceId` struct — see
/// `panda_abi::device`) against known and future devices.
///
/// If `mailbox` is non-zero, the returned subscription handle is attached
/// to it with `EVENT_DEVICE_ADDED | EVENT_DEVICE_REMOVED`; pass
/// `Handle::from(0u64)` to skip attachment. Immediately replays
/// `EVENT_DEVICE_ADDED` for every currently-known matching device.
#[inline(always)]
pub fn device_subscribe(bus_type: BusType, match_data: &[u8], mailbox: Handle) -> Result<Handle> {
    error::from_syscall_handle(sys::device::subscribe(bus_type.as_u32(), match_data, mailbox))
}

/// Claim a device using a token received via `EVENT_DEVICE_ADDED`. Consumes
/// the token; returns the owned device handle on success.
#[inline(always)]
pub fn device_claim(token: Handle) -> Result<Handle> {
    error::from_syscall_handle(sys::device::claim(token))
}

/// Map a claimed device's MMIO BAR into the caller's address space.
///
/// **Not implemented**: requires IOMMU-aware page table setup (Phase 6 of
/// the device driver model plan). Always returns `NotSupported`.
#[inline(always)]
pub fn device_map_mmio(_device: Handle, _bar_index: u32) -> Result<MmioRegion> {
    Err(panda_abi::ErrorCode::NotSupported)
}

/// Allocate IOMMU-mapped, physically contiguous DMA memory for a claimed
/// device, returning `(virt_addr, iova)`.
///
/// **Not implemented**: requires IOMMU support (Phase 6). Always returns
/// `NotSupported`.
#[inline(always)]
pub fn dma_alloc(_device: Handle, _size: usize) -> Result<(usize, u64)> {
    Err(panda_abi::ErrorCode::NotSupported)
}

/// Free memory allocated by [`dma_alloc`].
///
/// **Not implemented**: requires IOMMU support (Phase 6). Always returns
/// `NotSupported`.
#[inline(always)]
pub fn dma_free(_device: Handle, _virt_addr: usize, _size: usize) -> Result<()> {
    Err(panda_abi::ErrorCode::NotSupported)
}

/// Subscribe to IRQ events (`EVENT_DEVICE_IRQ`) for a claimed device.
///
/// **Not implemented**: requires IOMMU-aware IRQ routing setup (Phase 6).
/// Always returns `NotSupported`.
#[inline(always)]
pub fn device_subscribe_irq(_device: Handle, _mailbox: Handle) -> Result<()> {
    Err(panda_abi::ErrorCode::NotSupported)
}

/// A bounds-checked, volatile-only window onto a device's mapped MMIO
/// region.
///
/// `read`/`write` are the only way to access device registers from driver
/// code: there is no way to form a `&`/`&mut` reference to device memory
/// through this type, which would otherwise let the compiler cache reads or
/// reorder writes across what looks like plain memory accesses but is
/// actually hardware with side effects.
///
/// Deliberately `Send` but not `Sync` — concurrent access from multiple
/// threads must be coordinated by the driver (e.g. a lock around the
/// region), since nothing here serialises `read`/`write` calls.
///
/// Bounds are enforced on every access:
///
/// ```
/// # use libpanda::device::MmioRegion;
/// let mut buf = [0u8; 16];
/// let region = unsafe { MmioRegion::new(buf.as_mut_ptr(), buf.len()) };
///
/// // In bounds: succeeds.
/// region.write::<u32>(0, 0xDEAD_BEEF);
/// assert_eq!(region.read::<u32>(0), 0xDEAD_BEEF);
///
/// // Exactly at the boundary (12 + size_of::<u32>() == 16): succeeds.
/// region.write::<u32>(12, 0x1234_5678);
/// assert_eq!(region.read::<u32>(12), 0x1234_5678);
/// ```
///
/// One byte past the end panics:
///
/// ```should_panic
/// # use libpanda::device::MmioRegion;
/// let mut buf = [0u8; 16];
/// let region = unsafe { MmioRegion::new(buf.as_mut_ptr(), buf.len()) };
/// let _ = region.read::<u32>(13); // 13 + 4 == 17 > 16
/// ```
pub struct MmioRegion {
    base: *mut u8,
    size: usize,
}

// Safety: `MmioRegion` only ever performs bounds-checked volatile accesses
// through raw-pointer arithmetic; it holds no thread-local state, so moving
// it to another thread is sound. It is intentionally not `Sync` (see the
// type's doc comment).
unsafe impl Send for MmioRegion {}

impl MmioRegion {
    /// Construct an `MmioRegion` over `[base, base + size)`.
    ///
    /// # Safety
    /// `base` must be a valid pointer to at least `size` bytes of mapped
    /// device MMIO space, valid for the lifetime of this `MmioRegion` and
    /// not aliased by any other live reference. Only
    /// `device_map_mmio` (once implemented) should construct one of these
    /// from the kernel's returned mapping.
    pub unsafe fn new(base: *mut u8, size: usize) -> Self {
        Self { base, size }
    }

    /// The size of the mapped region in bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Read a value from the given byte offset.
    ///
    /// # Panics
    /// Panics if the access would exceed the region's bounds.
    pub fn read<T: Copy>(&self, offset: usize) -> T {
        assert!(offset + core::mem::size_of::<T>() <= self.size);
        // Safety: bounds-checked above; `base` is valid for `size` bytes
        // per the safety contract of `new`.
        unsafe { core::ptr::read_volatile(self.base.add(offset) as *const T) }
    }

    /// Write a value to the given byte offset.
    ///
    /// # Panics
    /// Panics if the access would exceed the region's bounds.
    pub fn write<T: Copy>(&self, offset: usize, value: T) {
        assert!(offset + core::mem::size_of::<T>() <= self.size);
        // Safety: bounds-checked above; `base` is valid for `size` bytes
        // per the safety contract of `new`.
        unsafe { core::ptr::write_volatile(self.base.add(offset) as *mut T, value) }
    }
}

/// Declare a driver's PCI device match table, emitted into the
/// `.panda_devices.pci` ELF section the service manager scans at boot.
///
/// ```ignore
/// panda::pci_device_table![
///     { vendor: 0x1AF4, device: 0x1052 },
/// ];
/// ```
///
/// Unlike the design doc's literal macro body (which sizes the backing
/// array with the unstable `${count($v)}` meta-variable expression), this
/// counts repetitions with a stable-Rust const-eval trick (building a
/// zero-sized `[(); N]` from the same repetition and taking its `.len()`)
/// to avoid depending on an unstable macro feature. The emitted section
/// contents are identical either way.
#[macro_export]
macro_rules! pci_device_table {
    ($({ vendor: $v:expr, device: $d:expr }),+ $(,)?) => {
        #[unsafe(link_section = ".panda_devices.pci")]
        #[used]
        static _PANDA_PCI_DEVICES: [$crate::device::PciDeviceId; { [$($crate::__device_table_unit!($v)),+].len() }] = [
            $($crate::device::PciDeviceId {
                vendor_id: $v,
                device_id: $d,
                class: 0,
                class_mask: 0,
            }),+
        ];
    };
}

/// Declare a driver's USB device match table (`.panda_devices.usb`).
///
/// ```ignore
/// panda::usb_device_table![
///     { vendor: 0xFFFF, product: 0xFFFF,
///       device_class: 0x03, match_flags: panda::device::USB_MATCH_CLASS },
/// ];
/// ```
#[macro_export]
macro_rules! usb_device_table {
    ($({ vendor: $v:expr, product: $p:expr, device_class: $c:expr, match_flags: $f:expr }),+ $(,)?) => {
        #[unsafe(link_section = ".panda_devices.usb")]
        #[used]
        static _PANDA_USB_DEVICES: [$crate::device::UsbDeviceId; { [$($crate::__device_table_unit!($v)),+].len() }] = [
            $($crate::device::UsbDeviceId {
                vendor_id: $v,
                product_id: $p,
                device_class: $c,
                device_subclass: 0,
                device_protocol: 0,
                match_flags: $f,
                _pad: 0,
            }),+
        ];
    };
}

/// Declare a driver's ACPI device match table (`.panda_devices.acpi`).
///
/// ```ignore
/// panda::acpi_device_table![{ hid: b"PNP0501\0" }];
/// ```
#[macro_export]
macro_rules! acpi_device_table {
    ($({ hid: $hid:expr }),+ $(,)?) => {
        #[unsafe(link_section = ".panda_devices.acpi")]
        #[used]
        static _PANDA_ACPI_DEVICES: [$crate::device::AcpiDeviceId; { [$($crate::__device_table_unit!($hid)),+].len() }] = [
            $($crate::device::AcpiDeviceId { hid: $hid }),+
        ];
    };
}

/// Declare a driver's I/O port device match table (`.panda_devices.ioport`).
///
/// ```ignore
/// panda::ioport_device_table![{ base: 0x3F8, size: 8 }];
/// ```
#[macro_export]
macro_rules! ioport_device_table {
    ($({ base: $base:expr, size: $size:expr }),+ $(,)?) => {
        #[unsafe(link_section = ".panda_devices.ioport")]
        #[used]
        static _PANDA_IOPORT_DEVICES: [$crate::device::IoPortDeviceId; { [$($crate::__device_table_unit!($base)),+].len() }] = [
            $($crate::device::IoPortDeviceId { base: $base, size: $size, _pad: 0 }),+
        ];
    };
}

/// Implementation detail of the `*_device_table!` macros: maps any
/// repeated expression to `()`, so counting repetitions reduces to taking
/// the length of a `[(); N]` array built from the same repetition.
#[macro_export]
#[doc(hidden)]
macro_rules! __device_table_unit {
    ($_:expr) => {
        ()
    };
}
