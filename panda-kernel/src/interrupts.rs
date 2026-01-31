use core::{
    arch::asm,
    sync::atomic::{AtomicUsize, Ordering},
};

use log::{debug, error};
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
}

/// IRQ base vector offset (IRQs 0-15 map to vectors 0x20-0x2F)
const IRQ_BASE_VECTOR: u8 = 0x20;

/// Set a handler for an IRQ line (0-255).
///
/// IRQ lines are mapped to interrupt vectors starting at 0x20.
/// Pass `None` to restore the default handler (which just sends EOI).
pub fn set_irq_handler(irq: u8, handler: Option<IrqHandlerFunc>) {
    let vector = IRQ_BASE_VECTOR.saturating_add(irq);

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

/// Set a handler for a specific interrupt vector (0-255).
///
/// Unlike `set_irq_handler`, this does not add IRQ_BASE_VECTOR offset.
/// Use this for MSI/MSI-X interrupts that deliver to specific vectors.
/// Pass `None` to restore the default handler (which just sends EOI).
pub fn set_interrupt_handler(vector: u8, handler: Option<IrqHandlerFunc>) {
    let mut descriptor_table = DESCRIPTOR_TABLE.write();
    let handler = handler.unwrap_or(default_irq_handler);
    let kernel_cs = SegmentSelector::new(1, PrivilegeLevel::Ring0);
    unsafe {
        descriptor_table[vector]
            .set_handler_fn(handler)
            .set_code_selector(kernel_cs);
    }
}

/// Set a raw handler for an IRQ line (naked function, not x86-interrupt).
///
/// Used for handlers that need full register control, such as context switching
/// where we need to save/restore all GPRs.
pub fn set_raw_handler(irq: u8, handler: u64) {
    let vector = IRQ_BASE_VECTOR.saturating_add(irq);

    let mut descriptor_table = DESCRIPTOR_TABLE.write();
    let kernel_cs = SegmentSelector::new(1, PrivilegeLevel::Ring0);
    unsafe {
        descriptor_table[vector]
            .set_handler_addr(x86_64::VirtAddr::new(handler))
            .set_code_selector(kernel_cs);
    }
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
    use x86_64::VirtAddr;

    let fault_address =
        Cr2::read().expect("CR2 contained non-canonical address while handling page fault");

    // Try demand paging for userspace memory access
    if error_code.contains(PageFaultErrorCode::USER_MODE) {
        // Only attempt demand paging for not-present faults.
        // If PROTECTION_VIOLATION is set, the page exists but access was denied
        // (e.g., write to read-only page) — this must not trigger demand paging.
        if !error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION) {
            // Try stack first (more common for initial faults)
            if crate::memory::try_handle_stack_page_fault(VirtAddr::new(fault_address.as_u64())) {
                return;
            }

            // Try heap
            let brk = crate::scheduler::with_current_process(|proc| proc.brk());
            if crate::memory::try_handle_heap_page_fault(
                VirtAddr::new(fault_address.as_u64()),
                brk,
            ) {
                return;
            }
        }

        // Unhandled user-mode page fault: either a protection violation or
        // a not-present fault outside valid memory regions. Kill the process.
        let current_pid = crate::scheduler::current_process_id();
        let cause = if error_code.contains(PageFaultErrorCode::CAUSED_BY_WRITE) { "write" } else { "read" };
        let kind = if error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION) { "protection violation" } else { "invalid address" };
        error!(
            "<<<PROCESS_FAULT>>> Page fault in process {:?}: address={fault_address:#x}, caused by {cause}, {kind} — killing process",
            current_pid,
        );
        let process_info =
            crate::scheduler::with_current_process(|proc| proc.info().clone());
        crate::scheduler::remove_process(current_pid);
        process_info.set_exit_code(1);
        unsafe {
            crate::scheduler::exec_next_runnable();
        }
    }

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
