#![no_main]
#![no_std]
#![feature(iter_collect_into)]
#![feature(const_default)]
#![feature(const_trait_impl)]
#![feature(iter_advance_by)]
#![feature(iter_array_chunks)]
#![feature(abi_x86_interrupt)]
#![feature(ptr_cast_array)]
#![feature(ptr_as_ref_unchecked)]
extern crate alloc;

mod acpi;
mod devices;
mod exec;
mod interrupts;
mod logging;
mod memory;
mod panic;
mod pci;
mod syscall;
mod uefi;

use ::uefi::{Status, entry};
use log::info;

use crate::{exec::exec_raw, logging::Logger};

static LOGGER: Logger = Logger;

#[entry]
fn main() -> Status {
    LOGGER.init();
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Debug);

    let uefi_info = uefi::init_and_exit_boot_services();

    info!("Panda");

    unsafe {
        memory::init_from_uefi(&uefi_info.memory_map);
    }

    acpi::init(uefi_info.acpi2_rsdp.expect("No ACPI2 RSDP"));
    syscall::init();
    interrupts::init();
    pci::init();
    devices::init();

    exec_raw(uefi_info.init_program);
}
