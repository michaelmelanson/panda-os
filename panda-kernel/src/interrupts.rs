use core::{
    arch::asm,
    sync::atomic::{AtomicUsize, Ordering},
};

use log::debug;
use spinning_top::RwSpinlock;
use x86_64::{
    PrivilegeLevel,
    instructions::interrupts,
    registers::control::Cr2,
    structures::{
        gdt::SegmentSelector,
        idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode},
    },
};

static BREAKPOINT_INTERRUPT_COUNT: AtomicUsize = AtomicUsize::new(0);

pub use x86_64::structures::idt::PageFaultHandlerFunc;

/// Type alias for IRQ handler functions.
pub type IrqHandlerFunc = extern "x86-interrupt" fn(InterruptStackFrame);

static DESCRIPTOR_TABLE: RwSpinlock<InterruptDescriptorTable> =
    RwSpinlock::new(InterruptDescriptorTable::new());

/// Set a custom page fault handler. Pass `None` to restore the default handler.
pub fn set_page_fault_handler(handler: Option<PageFaultHandlerFunc>) {
    let mut descriptor_table = DESCRIPTOR_TABLE.write();
    let handler = handler.unwrap_or(default_page_fault_handler);
    let kernel_cs = SegmentSelector::new(1, PrivilegeLevel::Ring0);
    unsafe {
        descriptor_table
            .page_fault
            .set_handler_fn(handler)
            .set_code_selector(kernel_cs)
            .set_stack_index(1);
    }
    drop(descriptor_table);
}

/// Set a handler for an IRQ vector (0x20-0xFF).
///
/// Vector numbers 0x00-0x1F are reserved for CPU exceptions.
/// Pass `None` to restore the default handler (which just sends EOI).
pub fn set_irq_handler(vector: u8, handler: Option<IrqHandlerFunc>) {
    assert!(vector >= 0x20, "Vectors 0x00-0x1F are reserved for CPU exceptions");

    let mut descriptor_table = DESCRIPTOR_TABLE.write();
    let handler = handler.unwrap_or(default_irq_handler);
    let kernel_cs = SegmentSelector::new(1, PrivilegeLevel::Ring0);
    unsafe {
        descriptor_table[vector]
            .set_handler_fn(handler)
            .set_code_selector(kernel_cs);
    }
    drop(descriptor_table);
}

extern "x86-interrupt" fn default_irq_handler(_stack_frame: InterruptStackFrame) {
    crate::apic::eoi();
}

pub fn init() {
    let mut descriptor_table = DESCRIPTOR_TABLE.write();
    let kernel_cs = SegmentSelector::new(1, PrivilegeLevel::Ring0);

    unsafe {
        // 1 = 0x01
        descriptor_table
            .debug
            .set_handler_fn(debug_handler)
            .set_stack_index(1);

        // 3 = 0x03
        descriptor_table
            .breakpoint
            .set_handler_fn(breakpoint_handler)
            .set_privilege_level(PrivilegeLevel::Ring3)
            .set_code_selector(kernel_cs);

        // 6 = 0x06
        descriptor_table
            .invalid_opcode
            .set_handler_fn(invalid_opcode_handler)
            .set_code_selector(kernel_cs);

        // 8 = 0x08
        descriptor_table
            .double_fault
            .set_handler_fn(double_fault_handler)
            .set_code_selector(kernel_cs);

        // 13 = 0x0D
        descriptor_table
            .general_protection_fault
            .set_handler_fn(gpf_handler)
            .set_code_selector(kernel_cs)
            .set_stack_index(1);

        // 14 = 0x0E
        descriptor_table
            .page_fault
            .set_handler_fn(default_page_fault_handler)
            .set_code_selector(kernel_cs)
            .set_stack_index(1);
    }

    // Timer interrupt (vector 0x20) from Local APIC
    unsafe {
        descriptor_table[0x20]
            .set_handler_fn(default_irq_handler)
            .set_code_selector(kernel_cs);
    }
    drop(descriptor_table);

    unsafe {
        (*DESCRIPTOR_TABLE.data_ptr()).load();
    }

    interrupts::enable();

    interrupts::int3();
    let breakpoint_count = BREAKPOINT_INTERRUPT_COUNT.load(Ordering::SeqCst);
    assert_eq!(breakpoint_count, 1, "did not receive breakpoint interrupt");
}

extern "x86-interrupt" fn debug_handler(stack_frame: InterruptStackFrame) {
    debug!("DEBUG: {stack_frame:?}");
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    debug!("BREAKPOINT: {stack_frame:?}");
    BREAKPOINT_INTERRUPT_COUNT.fetch_add(1, Ordering::Relaxed);
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    panic!("Invalid opcode: {stack_frame:?}");
}

extern "x86-interrupt" fn gpf_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    panic!("General protection fault: error code {error_code}\n{stack_frame:?}");
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    let rax: usize;
    let rcx: usize;
    let rsi: usize;
    let rdi: usize;
    unsafe {
        asm!(
            "",
            out("rax") rax,
            out("rcx") rcx,
            out("rsi") rsi,
            out("rdi") rdi
        );
    }

    panic!(
        "Double fault:\n  rax={rax:#x} rcx={rcx:#x} rsi={rsi:#x} rdi={rdi:#x}\n  error code {error_code}\n{stack_frame:?}"
    );
}

extern "x86-interrupt" fn default_page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let fault_address =
        Cr2::read().expect("CR2 contained non-canonical address while handling page fault");

    panic!(
        "Page fault:\n  Fault address:   {fault_address:#020x}\n  Current address: {:#020x}\n  Stack pointer:   {:#020x}\n  Caused by {} while executing in {} mode ({error_code:?})",
        stack_frame.instruction_pointer,
        stack_frame.stack_pointer,
        if error_code.contains(PageFaultErrorCode::CAUSED_BY_WRITE) {
            "write"
        } else {
            "read"
        },
        if error_code.contains(PageFaultErrorCode::USER_MODE) {
            "user"
        } else {
            "kernel"
        }
    );
}
