//! Userspace device driver model: ELF device-match tables, `MmioRegion`,
//! and the `OP_DEVICE_*` syscall wrappers.
//!
//! See `plans/device-driver-model.md`. All seven syscall wrappers below
//! round-trip to real kernel handlers (`panda-kernel/src/syscall/device.rs`).

// `panda_abi::device::Handle` is just `u64`, distinct from (and not
// re-exported over) `crate::Handle`, libpanda's typed handle wrapper.
pub use panda_abi::device::{
    AcpiDeviceId, AcpiPath, BusType, DeviceEvent, DeviceIdentity, DmaAllocOut,
    EVENT_DEVICE_ADDED, EVENT_DEVICE_IRQ, EVENT_DEVICE_REMOVED, IoPortAddress, IoPortDeviceId,
    MmioMapOut, OP_DEVICE_CLAIM, OP_DEVICE_MAP_MMIO, OP_DEVICE_PCI_CONFIG_READ,
    OP_DEVICE_POLL_EVENT, OP_DEVICE_SUBSCRIBE, OP_DEVICE_SUBSCRIBE_IRQ, OP_DMA_ALLOC,
    OP_DMA_FREE, PCI_MATCH_ANY, PciAddress, PciDeviceId, USB_MATCH_CLASS, USB_MATCH_PRODUCT,
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

/// Drain one pending [`DeviceEvent`] off `subscription`'s queue.
///
/// Returns `Ok(Some(event))` if one was pending, `Ok(None)` if the queue is
/// empty. The mailbox events `device_subscribe` attaches only carry an
/// event bit (`EVENT_DEVICE_ADDED`/`EVENT_DEVICE_REMOVED`) — this is how a
/// subscriber reads back the actual bus/identity/claim-token payload after
/// waking up (e.g. via `Mailbox::wait`).
#[inline(always)]
pub fn device_poll_event(subscription: Handle) -> Result<Option<DeviceEvent>> {
    let mut out = DeviceEvent {
        bus_type: BusType::Pci,
        _pad: [0; 4],
        identity: DeviceIdentity {
            pci: PciAddress {
                segment: 0,
                bus: 0,
                device: 0,
                function: 0,
                _pad: [0; 3],
            },
        },
        token: 0,
    };
    let ret = sys::device::poll_event(subscription, &mut out);
    if ret < 0 {
        return Err(error::from_code(ret));
    }
    Ok(if ret == 1 { Some(out) } else { None })
}

/// Map a claimed device's MMIO BAR into the caller's address space.
#[inline(always)]
pub fn device_map_mmio(device: Handle, bar_index: u32) -> Result<MmioRegion> {
    let mut out = MmioMapOut::default();
    error::from_syscall(sys::device::map_mmio(device, bar_index, &mut out))?;
    // Safety: the kernel just mapped `out.size` bytes of device MMIO at
    // `out.virt_addr` into this process, and this is the only place a
    // pointer into that mapping is ever constructed.
    Ok(unsafe { MmioRegion::new(out.virt_addr as *mut u8, out.size as usize) })
}

/// Allocate IOMMU-mapped, physically contiguous DMA memory for a claimed
/// device, returning `(virt_addr, iova)`.
#[inline(always)]
pub fn dma_alloc(device: Handle, size: usize) -> Result<(usize, u64)> {
    let mut out = DmaAllocOut { virt_addr: 0, iova: 0 };
    error::from_syscall(sys::device::dma_alloc(device, size, &mut out))?;
    Ok((out.virt_addr as usize, out.iova))
}

/// Free memory allocated by [`dma_alloc`].
#[inline(always)]
pub fn dma_free(device: Handle, virt_addr: usize, size: usize) -> Result<()> {
    error::from_syscall_unit(sys::device::dma_free(device, virt_addr, size))
}

/// Subscribe to IRQ events (`EVENT_DEVICE_IRQ`) for a claimed device.
#[inline(always)]
pub fn device_subscribe_irq(device: Handle, mailbox: Handle) -> Result<()> {
    error::from_syscall_unit(sys::device::subscribe_irq(device, mailbox))
}

/// Read `width` (1, 2, or 4) bytes of raw PCI config space at `offset` from
/// a claimed device. See `OP_DEVICE_PCI_CONFIG_READ`'s doc comment for why
/// this exists (walking a virtio-pci capability list from userspace).
#[inline(always)]
pub fn device_pci_config_read(device: Handle, offset: u32, width: u32) -> Result<u32> {
    let ret = sys::device::pci_config_read(device, offset, width);
    if ret < 0 {
        return Err(error::from_code(ret));
    }
    Ok(ret as u32)
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
