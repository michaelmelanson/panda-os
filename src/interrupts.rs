use log::info;
use spinning_top::RwSpinlock;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

static DESCRIPTOR_TABLE: RwSpinlock<InterruptDescriptorTable> =
    RwSpinlock::new(InterruptDescriptorTable::new());

pub fn init() {
    let mut descriptor_table = DESCRIPTOR_TABLE.write();
    descriptor_table
        .general_protection_fault
        .set_handler_fn(gpf_handler);

    descriptor_table
        .double_fault
        .set_handler_fn(double_fault_handler);

    descriptor_table
        .page_fault
        .set_handler_fn(page_fault_handler);

    unsafe {
        descriptor_table.load_unsafe();
    }

    info!("Loaded IDT");
}

extern "x86-interrupt" fn gpf_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    panic!("General protection fault: error code {error_code}\n{stack_frame:?}");
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    panic!("Double fault: error code {error_code}\n{stack_frame:?}");
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    panic!("Page fault: error code {error_code:?}\n{stack_frame:?}");
}
