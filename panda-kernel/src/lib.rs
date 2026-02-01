#![no_std]
#![feature(iter_collect_into)]
#![feature(const_default)]
#![feature(const_trait_impl)]
#![feature(iter_advance_by)]
#![feature(iter_array_chunks)]
#![feature(abi_x86_interrupt)]
#![feature(ptr_cast_array)]
#![feature(ptr_as_ref_unchecked)]
#![feature(allocator_api)]
#![feature(negative_impls)]

extern crate alloc;

pub mod acpi;
pub mod apic;
pub mod compositor;
pub mod device_address;
pub mod device_path;
pub mod devices;
pub mod executor;
pub mod handle;
pub mod initrd;
pub mod interrupts;
pub mod logging;
pub mod memory;
pub mod pci;
pub mod process;
pub mod qemu;
pub mod resource;
pub mod scheduler;
pub mod syscall;
pub mod time;
pub mod uefi;
pub mod vfs;

#[cfg(feature = "testing")]
pub mod testing;

// Panic handler is defined in each binary (main.rs, tests/*) not in lib

use logging::Logger;

pub use qemu::{QemuExitCode, exit_qemu};
pub use uefi::UefiInfo;

static LOGGER: Logger = Logger;

/// Initialize kernel subsystems. Caller must call uefi::init() first.
/// Returns ACPI2 RSDP address for use after higher-half jump.
pub fn init() -> x86_64::PhysAddr {
    LOGGER.init();
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    let uefi_info = uefi::exit_boot_services();

    unsafe {
        memory::init_from_uefi(&uefi_info);
    }

    uefi_info.acpi2_rsdp.expect("No ACPI2 RSDP")
}

/// Continue kernel initialization after higher-half jump.
/// This initializes ACPI, syscall, interrupts, APIC, PCI, and devices.
pub fn init_after_higher_half_jump(acpi2_rsdp: x86_64::PhysAddr) {
    acpi::init(acpi2_rsdp);
    memory::smap::enable();
    syscall::init();
    interrupts::init();
    apic::init();
    pci::init();
    devices::init();
}

pub fn breakpoint() {
    // do nothing, just give an address to set breakpoints on in `.gdbinit`
}
