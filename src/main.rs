#![no_main]
#![no_std]
#![feature(iter_collect_into)]
#![feature(const_default)]
#![feature(const_trait_impl)]
#![feature(iter_advance_by)]
#![feature(iter_array_chunks)]
extern crate alloc;

mod acpi;
mod devices;
mod logging;
mod memory;
mod panic;
mod pci;
mod uefi;

use ::uefi::{Status, entry};
use log::info;

use crate::logging::Logger;

static LOGGER: Logger = Logger;

#[entry]
fn main() -> Status {
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Trace);

    let uefi_info = uefi::init_and_exit_boot_services();

    info!("Panda");

    unsafe {
        memory::init_from_uefi(&uefi_info.memory_map);
    }

    acpi::init(uefi_info.acpi2_rsdp.expect("No ACPI2 RSDP"));
    pci::init();
    devices::init();

    panic!("Reached end of main function");
}
