//! Test infrastructure for kernel integration tests.
//!
//! This module is only compiled when the `testing` feature is enabled.

use crate::{
    QemuExitCode, exit_qemu, init, init_after_higher_half_jump, memory, print, println, syscall,
};
use core::sync::atomic::{AtomicU64, Ordering};

/// Trait for test functions that can print their name.
pub trait Testable: Sync {
    fn run(&self);
}

impl<T: Fn() + Sync> Testable for T {
    fn run(&self) {
        print!("{}...\t", core::any::type_name::<T>());
        self();
        println!("[ok]");
    }
}

// Statics for passing data across higher-half jump
static ACPI2_RSDP: AtomicU64 = AtomicU64::new(0);
static TESTS_PTR: AtomicU64 = AtomicU64::new(0);
static TESTS_LEN: AtomicU64 = AtomicU64::new(0);

/// Initialize and run tests with higher-half jump.
///
/// This performs the full initialization sequence including the higher-half
/// transition, then runs the provided tests.
pub fn init_and_run_tests(tests: &'static [&'static dyn Testable]) -> ! {
    // Store tests pointer before the jump
    TESTS_PTR.store(tests.as_ptr() as u64, Ordering::SeqCst);
    TESTS_LEN.store(tests.len() as u64, Ordering::SeqCst);

    // Early init
    let acpi2_rsdp = init();
    ACPI2_RSDP.store(acpi2_rsdp.as_u64(), Ordering::SeqCst);

    // Get boot stack and jump
    let boot_stack_top = syscall::gdt::SYSCALL_STACK.inner.as_ptr() as u64
        + syscall::gdt::SYSCALL_STACK.inner.len() as u64;

    unsafe {
        memory::jump_to_higher_half(boot_stack_top, test_continuation);
    }
}

unsafe extern "C" fn test_continuation() -> ! {
    let acpi2_rsdp = x86_64::PhysAddr::new(ACPI2_RSDP.load(Ordering::SeqCst));
    init_after_higher_half_jump(acpi2_rsdp);

    // Reconstruct tests slice
    let tests_ptr = TESTS_PTR.load(Ordering::SeqCst) as *const &'static dyn Testable;
    let tests_len = TESTS_LEN.load(Ordering::SeqCst) as usize;
    let tests = unsafe { core::slice::from_raw_parts(tests_ptr, tests_len) };

    run_tests(tests);
}

/// Test runner that executes all test cases.
pub fn test_runner(tests: &[&dyn Testable]) {
    println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    println!();
    println!("All tests passed!");
    exit_qemu(QemuExitCode::Success);
}

/// Panic handler for tests - prints error and exits QEMU with failure.
pub fn test_panic_handler(info: &core::panic::PanicInfo) -> ! {
    println!("[failed]");
    println!();
    println!("Error: {}", info);
    exit_qemu(QemuExitCode::Failed);
}

/// Run tests and exit QEMU. Call this from test entry points.
pub fn run_tests(tests: &[&dyn Testable]) -> ! {
    test_runner(tests);
    // test_runner calls exit_qemu, but just in case:
    exit_qemu(QemuExitCode::Success);
}

/// Macro to generate test harness boilerplate.
///
/// Usage:
/// ```ignore
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
            static TESTS: &[&dyn $crate::testing::Testable] = &[$(&$test),*];
            $crate::testing::init_and_run_tests(TESTS);
        }

        #[panic_handler]
        fn panic(info: &core::panic::PanicInfo) -> ! {
            $crate::testing::test_panic_handler(info)
        }
    };
}
