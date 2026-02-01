#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use panda_kernel::interrupts;
use panda_kernel::memory::{self, MemoryMappingOptions, map_external, smap};
use x86_64::VirtAddr;
use x86_64::registers::control::{Cr2, Cr4, Cr4Flags};
use x86_64::structures::idt::{InterruptStackFrame, PageFaultErrorCode};

panda_kernel::test_harness!(
    smap_enabled_after_boot,
    smap_ac_flag_clear,
    smap_violation_causes_page_fault,
);

fn smap_enabled_after_boot() {
    let cr4 = Cr4::read();
    assert!(
        cr4.contains(Cr4Flags::SUPERVISOR_MODE_ACCESS_PREVENTION),
        "CR4.SMAP should be set after kernel init"
    );
    assert!(
        smap::is_enabled(),
        "smap::is_enabled() should return true"
    );
}

fn smap_ac_flag_clear() {
    // AC flag should be clear during normal kernel execution
    let rflags: u64;
    unsafe {
        core::arch::asm!("pushfq; pop {}", out(reg) rflags, options(nomem));
    }
    assert!(
        rflags & (1 << 18) == 0,
        "AC flag should be clear during kernel execution"
    );

    // with_userspace_access should temporarily set AC then clear it
    smap::with_userspace_access(|| {
        let rflags_inner: u64;
        unsafe {
            core::arch::asm!("pushfq; pop {}", out(reg) rflags_inner, options(nomem));
        }
        assert!(
            rflags_inner & (1 << 18) != 0,
            "AC flag should be set inside with_userspace_access"
        );
    });

    // AC should be clear again after the closure
    let rflags_after: u64;
    unsafe {
        core::arch::asm!("pushfq; pop {}", out(reg) rflags_after, options(nomem));
    }
    assert!(
        rflags_after & (1 << 18) == 0,
        "AC flag should be clear after with_userspace_access"
    );
}

// Test state for the SMAP violation test
static SMAP_FAULT_OCCURRED: AtomicBool = AtomicBool::new(false);
static SMAP_FAULT_ERROR_CODE: AtomicU64 = AtomicU64::new(0);
static SMAP_FAULT_ADDRESS: AtomicU64 = AtomicU64::new(0);

/// Custom page fault handler for the SMAP violation test.
/// Records the fault details and skips the faulting instruction.
extern "x86-interrupt" fn smap_test_page_fault_handler(
    mut stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let fault_address = Cr2::read().expect("CR2 contained non-canonical address");

    SMAP_FAULT_OCCURRED.store(true, Ordering::SeqCst);
    SMAP_FAULT_ERROR_CODE.store(error_code.bits(), Ordering::SeqCst);
    SMAP_FAULT_ADDRESS.store(fault_address.as_u64(), Ordering::SeqCst);

    // Skip the faulting `mov` instruction. We use a labeled asm block in the
    // test so we know the resume address. Here we simply advance RIP past the
    // faulting instruction. The faulting instruction is a `mov rax, [reg]`
    // which is a REX.W + opcode + modrm = 3 bytes. But the exact encoding
    // depends on the register allocator, so instead we use the recovery label
    // approach: the test writes the recovery address into SMAP_RECOVERY_RIP
    // before triggering the fault.
    unsafe {
        let recovery = SMAP_RECOVERY_RIP.load(Ordering::SeqCst);
        if recovery != 0 {
            stack_frame.as_mut().update(|frame| {
                frame.instruction_pointer = VirtAddr::new(recovery);
            });
        }
    }
}

static SMAP_RECOVERY_RIP: AtomicU64 = AtomicU64::new(0);

/// Test that reading a user-mapped page from kernel mode without stac/clac
/// triggers a page fault with PROTECTION_VIOLATION (i.e., an SMAP violation).
fn smap_violation_causes_page_fault() {
    // Allocate and map a page with user=true at a userspace address
    let frame = memory::allocate_frame();
    let phys_addr = frame.start_address();
    let virt_addr = VirtAddr::new(0x0000_7000_0000_0000);

    let _mapping = map_external(
        phys_addr,
        virt_addr,
        4096,
        MemoryMappingOptions {
            user: true,
            executable: false,
            writable: true,
        },
    );

    // Write a known value to the page (using stac/clac so this succeeds)
    smap::with_userspace_access(|| unsafe {
        core::ptr::write_volatile(virt_addr.as_mut_ptr::<u64>(), 0xCAFE_BABE_DEAD_BEEF);
    });

    // Reset test state
    SMAP_FAULT_OCCURRED.store(false, Ordering::SeqCst);
    SMAP_FAULT_ERROR_CODE.store(0, Ordering::SeqCst);
    SMAP_FAULT_ADDRESS.store(0, Ordering::SeqCst);
    SMAP_RECOVERY_RIP.store(0, Ordering::SeqCst);

    // Install our custom page fault handler
    interrupts::set_page_fault_handler(Some(smap_test_page_fault_handler));

    // Try to read from the user-mapped page WITHOUT stac/clac — this should
    // trigger an SMAP violation page fault.
    unsafe {
        core::arch::asm!(
            // Store the recovery address (the label "2:") into SMAP_RECOVERY_RIP
            "lea {tmp}, [rip + 2f]",
            "mov [{recovery}], {tmp}",
            // Attempt the forbidden read — SMAP should fault here
            "mov {tmp}, [{addr}]",
            // Recovery point — execution resumes here after the fault handler
            "2:",
            addr = in(reg) virt_addr.as_u64(),
            recovery = in(reg) &SMAP_RECOVERY_RIP as *const AtomicU64 as u64,
            tmp = out(reg) _,
            options(nostack),
        );
    }

    // Restore default handler
    interrupts::set_page_fault_handler(None);

    // Verify that a page fault occurred
    assert!(
        SMAP_FAULT_OCCURRED.load(Ordering::SeqCst),
        "Expected a page fault when reading user-mapped page without stac/clac, but none occurred"
    );

    // Verify the fault was at the expected address
    let fault_addr = SMAP_FAULT_ADDRESS.load(Ordering::SeqCst);
    assert_eq!(
        fault_addr,
        virt_addr.as_u64(),
        "Page fault occurred at wrong address: expected {:#x}, got {:#x}",
        virt_addr.as_u64(),
        fault_addr
    );

    // Verify the error code indicates an SMAP violation:
    // - PROTECTION_VIOLATION must be set (page is present but SMAP denied access)
    // - USER_MODE must NOT be set (we're in kernel mode)
    // - INSTRUCTION_FETCH must NOT be set (this was a data read, not instruction fetch)
    let error_code =
        PageFaultErrorCode::from_bits_truncate(SMAP_FAULT_ERROR_CODE.load(Ordering::SeqCst));

    assert!(
        error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION),
        "SMAP violation should have PROTECTION_VIOLATION flag set, got: {:?}",
        error_code
    );

    assert!(
        !error_code.contains(PageFaultErrorCode::USER_MODE),
        "SMAP violation should NOT have USER_MODE flag (we're in kernel mode), got: {:?}",
        error_code
    );

    assert!(
        !error_code.contains(PageFaultErrorCode::INSTRUCTION_FETCH),
        "SMAP violation should NOT have INSTRUCTION_FETCH flag (this was a data read), got: {:?}",
        error_code
    );
}
