use core::ptr::slice_from_raw_parts_mut;

use uefi::{
    CStr16,
    boot::{self, AllocateType, MemoryType},
    mem::memory_map::MemoryMapOwned,
    proto::media::file::{File, FileAttribute, FileInfo, FileMode},
};
use x86_64::PhysAddr;

pub struct UefiInfo {
    pub acpi2_rsdp: Option<PhysAddr>,
    pub memory_map: MemoryMapOwned,
    pub init_program: *const [u8],
}

pub fn init_and_exit_boot_services() -> UefiInfo {
    ::uefi::helpers::init().unwrap();

    let system_table = ::uefi::table::system_table_raw().expect("No UEFI system table");
    let system_table = unsafe { system_table.as_ref() };

    // this should work, in conjunction with code in `.gdbinit` to calculate an offset
    // but it seems to give the wrong value
    // {
    //     let loaded_image_handle = get_handle_for_protocol::<LoadedImage>().unwrap();
    //     let loaded_image =
    //         ::uefi::boot::open_protocol_exclusive::<LoadedImage>(loaded_image_handle).unwrap();
    //     let (base_address, _) = loaded_image.info();
    //     let base_address = base_address as usize;
    //     let address: usize;
    //     unsafe { core::arch::asm!("lea {}, [rip]", out(reg) address) }
    //     log::info!("Current address: {address:X?}");
    //     log::info!("Base address: {base_address:X?}");

    //     unsafe {
    //         let care_package_ptr = &mut *(0x55420 as *mut [usize; 2]);
    //         care_package_ptr[1] = base_address;
    //         care_package_ptr[0] = 0xCA5ECA5E;
    //     }
    // }

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

    let init_program = load_file("\\efi\\init");

    let memory_map = unsafe { uefi::boot::exit_boot_services(None) };

    UefiInfo {
        acpi2_rsdp,
        memory_map,
        init_program,
    }
}

fn load_file(path: &str) -> *const [u8] {
    let mut buf = [0u16; 255];
    let path =
        CStr16::from_str_with_buf(&path, &mut buf).expect("could not convert path to CStr16");

    let mut fs =
        boot::get_image_file_system(boot::image_handle()).expect("could not load file sytem");

    let mut root = fs.open_volume().expect("failed to open volume");
    let handle = root
        .open(&path, FileMode::Read, FileAttribute::empty())
        .expect("failed to open file");

    let mut buf = [0u8; 256];
    let mut file = handle
        .into_regular_file()
        .expect("file should be a regular file");
    let file_info = file
        .get_info::<FileInfo>(&mut buf)
        .expect("could not get file info");

    let file_size = file_info.file_size();
    let page_count = file_size.div_ceil(4096);

    let data = uefi::boot::allocate_pages(
        AllocateType::AnyPages,
        MemoryType::LOADER_DATA,
        page_count as usize,
    )
    .expect("could not allocate memory for file data");

    let data = slice_from_raw_parts_mut(data.as_ptr(), file_size as usize);

    file.read(unsafe { data.as_mut_unchecked() })
        .expect("could not read file contents");

    data
}
