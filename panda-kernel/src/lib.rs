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
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

pub mod acpi;
pub mod apic;
pub mod context;
pub mod devices;
pub mod handle;
pub mod initrd;
pub mod interrupts;
pub mod logging;
pub mod memory;
pub mod pci;
pub mod process;
pub mod qemu;
pub mod scheduler;
pub mod syscall;
pub mod uefi;
pub mod vfs;

// Panic handler is defined in each binary (main.rs, tests/*) not in lib

use logging::Logger;

pub use qemu::{QemuExitCode, exit_qemu};
pub use uefi::UefiInfo;

static LOGGER: Logger = Logger;

/// Initialize kernel subsystems. Caller must call uefi::init() first.
pub fn init() {
    LOGGER.init();
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Debug);

    let uefi_info = uefi::exit_boot_services();

    unsafe {
        memory::init_from_uefi(&uefi_info.memory_map);
    }

    acpi::init(uefi_info.acpi2_rsdp.expect("No ACPI2 RSDP"));
    syscall::init();
    interrupts::init();
    apic::init();
    scheduler::init_preemption();
    pci::init();
    devices::init();
}

/// Trait for test functions that can print their name
pub trait Testable {
    fn run(&self);
}

impl<T: Fn()> Testable for T {
    fn run(&self) {
        print!("{}...\t", core::any::type_name::<T>());
        self();
        println!("[ok]");
    }
}

/// Test runner that executes all test cases
pub fn test_runner(tests: &[&dyn Testable]) {
    println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    println!();
    println!("All tests passed!");
    exit_qemu(QemuExitCode::Success);
}

/// Panic handler for tests - prints error and exits QEMU with failure
pub fn test_panic_handler(info: &core::panic::PanicInfo) -> ! {
    println!("[failed]");
    println!();
    println!("Error: {}", info);
    exit_qemu(QemuExitCode::Failed);
}

pub fn breakpoint() {
    // do nothing, just give an address to set breakpoints on in `.gdbinit`
}

/// Run tests and exit QEMU. Call this from test entry points.
pub fn run_tests(tests: &[&dyn Testable]) -> ! {
    test_runner(tests);
    // test_runner calls exit_qemu, but just in case:
    exit_qemu(QemuExitCode::Success);
}

/// Macro to generate test harness boilerplate.
/// Usage:
/// ```
/// panda_kernel::test_harness!(test1, test2, test3);
/// ```
#[macro_export]
macro_rules! test_harness {
    ($($test:ident),* $(,)?) => {
        #[unsafe(no_mangle)]
        pub extern "efiapi" fn efi_main(
            image: ::uefi::Handle,
            system_table: *const core::ffi::c_void,
        ) -> ::uefi::Status {
            unsafe {
                ::uefi::boot::set_image_handle(image);
                ::uefi::table::set_system_table(system_table.cast());
            }
            $crate::uefi::init();
            $crate::init();
            let tests: &[&dyn $crate::Testable] = &[$(&$test),*];
            $crate::run_tests(tests);
        }

        #[panic_handler]
        fn panic(info: &core::panic::PanicInfo) -> ! {
            $crate::test_panic_handler(info)
        }
    };
}
