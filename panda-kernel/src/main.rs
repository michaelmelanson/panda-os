#![no_main]
#![no_std]

extern crate alloc;

use core::sync::atomic::{AtomicU64, Ordering};

use ::uefi::{Status, entry};
use log::info;
use panda_kernel::{
    initrd, memory,
    process::{Context, Process},
    resource, scheduler,
    syscall::gdt::SYSCALL_STACK,
    uefi, vfs,
};

/// ACPI2 RSDP address, stored before higher-half jump for use after.
static ACPI2_RSDP: AtomicU64 = AtomicU64::new(0);

/// Initrd data pointer, stored before higher-half jump.
static INITRD_DATA: AtomicU64 = AtomicU64::new(0);
static INITRD_LEN: AtomicU64 = AtomicU64::new(0);

#[entry]
fn main() -> Status {
    uefi::init();

    // Load initrd before exiting boot services
    let initrd_data = uefi::load_initrd();
    let (initrd_ptr, initrd_len) = unsafe {
        let slice = &*initrd_data;
        (slice.as_ptr(), slice.len())
    };
    INITRD_DATA.store(initrd_ptr as u64, Ordering::SeqCst);
    INITRD_LEN.store(initrd_len as u64, Ordering::SeqCst);

    // Early init: memory subsystem, physical window, kernel relocation
    let acpi2_rsdp = panda_kernel::init();
    ACPI2_RSDP.store(acpi2_rsdp.as_u64(), Ordering::SeqCst);

    // Get the boot stack address (top of SYSCALL_STACK)
    let boot_stack_top = SYSCALL_STACK.inner.as_ptr() as u64 + SYSCALL_STACK.inner.len() as u64;

    // Jump to higher-half execution
    unsafe {
        memory::jump_to_higher_half(boot_stack_top, higher_half_continuation);
    }
}

/// Continuation function called after jumping to higher-half.
/// This runs from the relocated kernel at higher-half addresses.
unsafe extern "C" fn higher_half_continuation() -> ! {
    // Continue initialization with ACPI, syscall, interrupts, etc.
    let acpi2_rsdp = x86_64::PhysAddr::new(ACPI2_RSDP.load(Ordering::SeqCst));
    panda_kernel::init_after_higher_half_jump(acpi2_rsdp);

    // Reconstruct initrd pointer - translate from identity-mapped to physical window
    // The identity-mapped address IS the physical address, so we can use it directly
    let initrd_phys = INITRD_DATA.load(Ordering::SeqCst);
    let initrd_len = INITRD_LEN.load(Ordering::SeqCst) as usize;
    let initrd_virt = memory::physical_address_to_virtual(x86_64::PhysAddr::new(initrd_phys));
    let initrd_data: *const [u8] =
        core::ptr::slice_from_raw_parts(initrd_virt.as_ptr(), initrd_len);

    // Remove identity mapping now that we're running in higher-half
    // and all pointers have been translated to use the physical window
    unsafe { memory::remove_identity_mapping() };

    initrd::init(initrd_data);

    // Mount initrd as /initrd
    let tarfs = vfs::TarFs::from_tar_data(initrd_data);
    vfs::mount("/initrd", alloc::boxed::Box::new(tarfs));

    // Initialize resource scheme system
    resource::init_schemes();

    info!("Panda OS");

    let init_data = initrd::get_init();
    let init_process =
        unsafe { Process::from_elf_data(Context::from_current_page_table(), init_data) };
    scheduler::init(init_process);

    // Start compositor task now that scheduler is ready
    panda_kernel::compositor::spawn_compositor_task();

    unsafe { scheduler::exec_next_runnable() };
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
