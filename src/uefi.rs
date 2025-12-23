use uefi::mem::memory_map::MemoryMapOwned;
use x86_64::PhysAddr;

pub struct UefiInfo {
    pub acpi2_rsdp: Option<PhysAddr>,
    pub memory_map: MemoryMapOwned,
}

pub fn init_and_exit_boot_services() -> UefiInfo {
    ::uefi::helpers::init().unwrap();

    let system_table = ::uefi::table::system_table_raw().expect("No UEFI system table");
    let system_table = unsafe { system_table.as_ref() };

    let mut acpi2_rsdp = None;

    for i in 0..system_table.number_of_configuration_table_entries as isize {
        let config_table = unsafe { system_table.configuration_table.offset(i) };
        let config_table_ref = unsafe {
            config_table
                .as_ref()
                .expect("Could not get UEFI config table at index {i}")
        };

        match config_table_ref.vendor_guid {
            uefi::table::cfg::ConfigTableEntry::ACPI2_GUID => {
                acpi2_rsdp = Some(PhysAddr::new(config_table_ref.vendor_table as u64));
            }
            _ => {}
        }
    }

    let memory_map = unsafe { uefi::boot::exit_boot_services(None) };

    UefiInfo {
        acpi2_rsdp,
        memory_map,
    }
}
