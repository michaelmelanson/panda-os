#![no_main]
#![no_std]

extern crate alloc;

use ::uefi::{Status, entry};
use log::info;
use panda_kernel::{
    initrd,
    process::{Context, Process},
    resource, scheduler, uefi, vfs,
};

#[entry]
fn main() -> Status {
    uefi::init();
    let initrd_data = uefi::load_initrd();
    panda_kernel::init();

    initrd::init(initrd_data);

    // Mount initrd as /initrd
    let tarfs = vfs::TarFs::from_tar_data(initrd_data);
    vfs::mount("/initrd", alloc::boxed::Box::new(tarfs));

    // Initialize resource scheme system
    resource::init_schemes();

    info!("Panda OS");

    unsafe {
        let init_data = initrd::get_init();
        let init_process = Process::from_elf_data(Context::from_current_page_table(), init_data);
        scheduler::init(init_process);

        // Start compositor task now that scheduler is ready
        panda_kernel::compositor::spawn_compositor_task();

        scheduler::exec_next_runnable();
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use log::error;
    use x86_64::instructions::hlt;

    let file = info.location().map(|l| l.file()).unwrap_or("unknown");
    let line = info.location().map(|l| l.line()).unwrap_or(0);

    error!("PANIC at [{}:{}]:\n{}", file, line, info.message());
    panda_kernel::breakpoint();
    loop {
        hlt();
    }
}
