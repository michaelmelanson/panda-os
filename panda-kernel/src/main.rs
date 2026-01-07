#![no_main]
#![no_std]

use log::info;
use panda_kernel::{context::Context, process::Process, scheduler};
use uefi::{Status, entry};

#[entry]
fn main() -> Status {
    let uefi_info = panda_kernel::init();

    info!("Panda");

    unsafe {
        let process =
            Process::from_elf_data(Context::from_current_page_table(), uefi_info.init_program);

        scheduler::add_process(process);
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
