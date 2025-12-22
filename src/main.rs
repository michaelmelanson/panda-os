#![no_main]
#![no_std]
extern crate alloc;

mod logging;
mod memory;
mod panic;

use log::info;
use uefi::{Status, entry};

use crate::logging::Logger;

static LOGGER: Logger = Logger;

#[entry]
fn main() -> Status {
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Trace);

    uefi::helpers::init().unwrap();

    info!("Panda");

    let memory_map = unsafe { uefi::boot::exit_boot_services(None) };
    info!("Exited boot services");

    unsafe {
        memory::init_from_uefi(&memory_map);
    }

    panic!("Reached end of main function");
}
