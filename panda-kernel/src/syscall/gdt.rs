//! GDT, TSS, and segment selector management.

use core::sync::atomic::{AtomicU16, Ordering};

use spinning_top::Spinlock;
use x86_64::{
    VirtAddr,
    instructions::tables::load_tss,
    registers::segmentation::{CS, DS, SS, Segment},
    structures::{
        gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector},
        tss::TaskStateSegment,
    },
};

static GDT: Spinlock<GlobalDescriptorTable> = Spinlock::new(GlobalDescriptorTable::new());
static TSS: Spinlock<TaskStateSegment> = Spinlock::new(TaskStateSegment::new());

/// Segment selectors set during GDT initialization.
static KERNEL_CS_SELECTOR: AtomicU16 = AtomicU16::new(0);
static KERNEL_DS_SELECTOR: AtomicU16 = AtomicU16::new(0);
static USER_CS_SELECTOR: AtomicU16 = AtomicU16::new(0);
static USER_DS_SELECTOR: AtomicU16 = AtomicU16::new(0);

/// Get the kernel code segment selector. Must be called after init().
pub fn kernel_code_selector() -> SegmentSelector {
    SegmentSelector(KERNEL_CS_SELECTOR.load(Ordering::Relaxed))
}

/// Get the kernel data segment selector. Must be called after init().
pub fn kernel_data_selector() -> SegmentSelector {
    SegmentSelector(KERNEL_DS_SELECTOR.load(Ordering::Relaxed))
}

/// Get the user code segment selector. Must be called after init().
pub fn user_code_selector() -> u16 {
    USER_CS_SELECTOR.load(Ordering::Relaxed)
}

/// Get the user code segment selector as SegmentSelector. Must be called after init().
pub fn user_cs_selector() -> SegmentSelector {
    SegmentSelector(USER_CS_SELECTOR.load(Ordering::Relaxed))
}

/// Get the user data segment selector. Must be called after init().
pub fn user_ds_selector() -> SegmentSelector {
    SegmentSelector(USER_DS_SELECTOR.load(Ordering::Relaxed))
}

#[repr(align(0x1000))]
pub struct KernelStack {
    pub inner: [u8; 0x10000], // 64KB kernel stack
}

/// Syscall handler stack - used by syscall_entry via manual RSP switch.
/// Also used as the boot stack for higher-half transition.
pub static SYSCALL_STACK: KernelStack = KernelStack {
    inner: [0; 0x10000],
};

/// Privilege level transition stack - used by CPU when transitioning ring 3 -> ring 0.
/// This is separate from SYSCALL_STACK so interrupts during syscall handling work correctly.
static PRIVILEGE_STACK: KernelStack = KernelStack {
    inner: [0; 0x10000],
};

/// User stack pointer storage for swapgs.
pub static USER_STACK_PTR: usize = 0x0badc0de;

/// Top of SYSCALL_STACK - initialized at runtime with the correct higher-half address.
/// Used by syscall_entry because inline assembly can't use lea with 64-bit addresses.
pub static mut SYSCALL_STACK_TOP: u64 = 0;

const INTERRUPT_STACK_SIZE: usize = 8192; // 8KB per interrupt stack

/// IST stacks for specific interrupt handlers (page fault, double fault, etc.)
static INTERRUPT_STACK_0: [u8; INTERRUPT_STACK_SIZE] = [0; INTERRUPT_STACK_SIZE];
static INTERRUPT_STACK_1: [u8; INTERRUPT_STACK_SIZE] = [0; INTERRUPT_STACK_SIZE];

/// Initialize the GDT, TSS, and segment selectors.
pub fn init() {
    // Initialize syscall stack top address for syscall_entry to use
    let syscall_stack_top = SYSCALL_STACK.inner.as_ptr() as u64 + SYSCALL_STACK.inner.len() as u64;
    unsafe {
        SYSCALL_STACK_TOP = syscall_stack_top;
    }
    log::debug!("GDT: syscall_stack_top = {:#x}", syscall_stack_top);

    let mut tss = TSS.lock();
    // Privilege stack table entries must point to the TOP of the stack (stacks grow downward)
    // This stack is used by the CPU for ring 3 -> ring 0 transitions (interrupts from userspace)
    let privilege_stack_top =
        PRIVILEGE_STACK.inner.as_ptr() as u64 + PRIVILEGE_STACK.inner.len() as u64;
    log::debug!("GDT: privilege_stack_top = {:#x}", privilege_stack_top);
    tss.privilege_stack_table[0] = VirtAddr::new(privilege_stack_top);
    tss.privilege_stack_table[1] = VirtAddr::new(privilege_stack_top);
    tss.privilege_stack_table[2] = VirtAddr::new(privilege_stack_top);
    // IST entries must point to the TOP of the stack (stacks grow downward)
    let ist0_top = INTERRUPT_STACK_0.as_ptr() as u64 + INTERRUPT_STACK_SIZE as u64;
    let ist1_top = INTERRUPT_STACK_1.as_ptr() as u64 + INTERRUPT_STACK_SIZE as u64;
    log::debug!("GDT: IST[0] = {:#x}, IST[1] = {:#x}", ist0_top, ist1_top);
    tss.interrupt_stack_table[0] = VirtAddr::new(ist0_top);
    tss.interrupt_stack_table[1] = VirtAddr::new(ist1_top);
    drop(tss);

    let mut gdt = GDT.lock();
    let kernel_cs = gdt.append(Descriptor::kernel_code_segment());
    let kernel_ds = gdt.append(Descriptor::kernel_data_segment());
    let tss_sel = gdt.append(Descriptor::tss_segment(unsafe { &*TSS.data_ptr() }));
    let user_ds = gdt.append(Descriptor::user_data_segment());
    let user_cs = gdt.append(Descriptor::user_code_segment());
    drop(gdt);

    // Store selectors for access by other modules
    KERNEL_CS_SELECTOR.store(kernel_cs.0, Ordering::Relaxed);
    KERNEL_DS_SELECTOR.store(kernel_ds.0, Ordering::Relaxed);
    USER_CS_SELECTOR.store(user_cs.0, Ordering::Relaxed);
    USER_DS_SELECTOR.store(user_ds.0, Ordering::Relaxed);

    unsafe {
        (*GDT.data_ptr()).load();
        CS::set_reg(kernel_cs);
        DS::set_reg(kernel_ds);
        SS::set_reg(kernel_ds);
        load_tss(tss_sel);
    }
}
