#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::VirtAddr;
use x86_64::registers::control::{Cr2, Efer, EferFlags};
use x86_64::structures::idt::{InterruptStackFrame, PageFaultErrorCode};
use panda_kernel::memory::{self, MemoryMappingOptions};
use panda_kernel::interrupts;

panda_kernel::test_harness!(
    efer_nxe_is_enabled,
    map_non_executable_page,
    map_executable_page,
    map_user_executable_page,
    execute_from_nx_page_faults
);

// Test state for the NX page fault test
static NX_FAULT_OCCURRED: AtomicBool = AtomicBool::new(false);
static NX_FAULT_ERROR_CODE: AtomicU64 = AtomicU64::new(0);
static NX_FAULT_ADDRESS: AtomicU64 = AtomicU64::new(0);

/// Verify that the NX enable bit is set in EFER
fn efer_nxe_is_enabled() {
    let efer = Efer::read();
    assert!(
        efer.contains(EferFlags::NO_EXECUTE_ENABLE),
        "IA32_EFER.NXE should be enabled after memory init"
    );
}

/// Test mapping a page with NO_EXECUTE flag set.
fn map_non_executable_page() {
    let frame = memory::allocate_frame();
    let phys_addr = frame.start_address();
    let virt_addr = VirtAddr::new(0x0000_4000_0000_0000);

    memory::map(
        phys_addr,
        virt_addr,
        4096,
        MemoryMappingOptions {
            user: false,
            executable: false,
            writable: true,
        },
    );

    // Verify the page is accessible for read/write
    let ptr = virt_addr.as_mut_ptr::<u64>();
    unsafe {
        core::ptr::write_volatile(ptr, 0xDEAD_BEEF_CAFE_BABE);
        let read_back = core::ptr::read_volatile(ptr);
        assert_eq!(read_back, 0xDEAD_BEEF_CAFE_BABE);
    }
}

/// Test mapping a page as executable (NO_EXECUTE flag NOT set).
fn map_executable_page() {
    let frame = memory::allocate_frame();
    let phys_addr = frame.start_address();
    let virt_addr = VirtAddr::new(0x0000_4001_0000_0000);

    memory::map(
        phys_addr,
        virt_addr,
        4096,
        MemoryMappingOptions {
            user: false,
            executable: true,  // NO_EXECUTE flag should NOT be set
            writable: false,
        },
    );

    // Verify the page is accessible for read
    let ptr = virt_addr.as_ptr::<u64>();
    unsafe {
        let _ = core::ptr::read_volatile(ptr);
    }
}

/// Test mapping a user-accessible executable page (like userspace code).
/// This simulates what happens when loading an ELF binary.
fn map_user_executable_page() {
    let frame = memory::allocate_frame();
    let phys_addr = frame.start_address();
    // Use a high virtual address that's not already mapped by UEFI
    // (UEFI uses huge pages for low memory which we can't break down yet)
    let virt_addr = VirtAddr::new(0x0000_5000_0000_0000);

    memory::map(
        phys_addr,
        virt_addr,
        4096,
        MemoryMappingOptions {
            user: true,
            executable: true,
            writable: false,
        },
    );

    // Verify the page is accessible for read from kernel mode
    let ptr = virt_addr.as_ptr::<u64>();
    unsafe {
        let _ = core::ptr::read_volatile(ptr);
    }
}

/// Custom page fault handler for the NX test.
/// Records the fault details and returns to a recovery point.
extern "x86-interrupt" fn nx_test_page_fault_handler(
    mut stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let fault_address = Cr2::read().expect("CR2 contained non-canonical address");

    NX_FAULT_OCCURRED.store(true, Ordering::SeqCst);
    NX_FAULT_ERROR_CODE.store(error_code.bits(), Ordering::SeqCst);
    NX_FAULT_ADDRESS.store(fault_address.as_u64(), Ordering::SeqCst);

    // Skip the faulting call instruction by advancing RIP
    // The call instruction that faulted was "call reg" which is 2 bytes (FF Dx)
    // But the fault occurred at the target address, so we need to return to
    // the instruction after the call. We stored the return address on stack.
    // Actually, for instruction fetch faults, RIP points to the faulting address.
    // We need to pop the return address from the stack and jump there.
    unsafe {
        // Read the return address from the stack (pushed by the call instruction)
        let rsp = stack_frame.stack_pointer.as_u64();
        let return_addr = *(rsp as *const u64);

        // Update the stack frame to return to the address after the call
        // and fix up the stack pointer (pop the return address)
        stack_frame.as_mut().update(|frame| {
            frame.instruction_pointer = VirtAddr::new(return_addr);
            frame.stack_pointer = VirtAddr::new(rsp + 8);
        });
    }
}

/// Test that attempting to execute code from a non-executable page causes a page fault.
/// This verifies that the NX bit actually provides protection.
fn execute_from_nx_page_faults() {
    let frame = memory::allocate_frame();
    let phys_addr = frame.start_address();
    let virt_addr = VirtAddr::new(0x0000_6000_0000_0000);

    // Map the page as writable so we can write code to it, but NOT executable
    memory::map(
        phys_addr,
        virt_addr,
        4096,
        MemoryMappingOptions {
            user: false,
            executable: false,  // NX bit should be set
            writable: true,
        },
    );

    // Write a simple RET instruction (0xC3) to the page
    let ptr = virt_addr.as_mut_ptr::<u8>();
    unsafe {
        core::ptr::write_volatile(ptr, 0xC3); // RET instruction
    }

    // Reset test state
    NX_FAULT_OCCURRED.store(false, Ordering::SeqCst);
    NX_FAULT_ERROR_CODE.store(0, Ordering::SeqCst);
    NX_FAULT_ADDRESS.store(0, Ordering::SeqCst);

    // Install our custom page fault handler
    interrupts::set_page_fault_handler(Some(nx_test_page_fault_handler));

    // Try to execute from the non-executable page - this should fault
    let target = virt_addr.as_u64();
    unsafe {
        core::arch::asm!(
            "call {target}",
            target = in(reg) target,
            clobber_abi("C"),
        );
    }

    // Restore default handler
    interrupts::set_page_fault_handler(None);

    // Verify that a page fault occurred
    assert!(
        NX_FAULT_OCCURRED.load(Ordering::SeqCst),
        "Expected a page fault when executing from NX page, but none occurred"
    );

    // Verify the fault was at the expected address
    let fault_addr = NX_FAULT_ADDRESS.load(Ordering::SeqCst);
    assert_eq!(
        fault_addr, virt_addr.as_u64(),
        "Page fault occurred at wrong address: expected {:#x}, got {:#x}",
        virt_addr.as_u64(), fault_addr
    );

    // Verify the error code indicates an NX violation specifically:
    // - INSTRUCTION_FETCH must be set (this was an instruction fetch)
    // - PROTECTION_VIOLATION must be set (page was present but access denied)
    // - CAUSED_BY_WRITE must NOT be set (this was a fetch, not a write)
    // - USER_MODE must NOT be set (we're in kernel mode)
    let error_code = PageFaultErrorCode::from_bits_truncate(
        NX_FAULT_ERROR_CODE.load(Ordering::SeqCst)
    );

    assert!(
        error_code.contains(PageFaultErrorCode::INSTRUCTION_FETCH),
        "Page fault should have INSTRUCTION_FETCH flag set, got: {:?}",
        error_code
    );

    assert!(
        error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION),
        "Page fault should have PROTECTION_VIOLATION flag set (page present but NX), got: {:?}",
        error_code
    );

    assert!(
        !error_code.contains(PageFaultErrorCode::CAUSED_BY_WRITE),
        "Page fault should NOT have CAUSED_BY_WRITE flag set, got: {:?}",
        error_code
    );

    assert!(
        !error_code.contains(PageFaultErrorCode::USER_MODE),
        "Page fault should NOT have USER_MODE flag set (we're in kernel), got: {:?}",
        error_code
    );
}
