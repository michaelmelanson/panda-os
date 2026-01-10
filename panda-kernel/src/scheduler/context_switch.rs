//! Context switching for preemptive multitasking.
//!
//! This module contains the naked assembly entry point for preemptable interrupts
//! and the logic for deciding when to preempt the current process.

use crate::apic;
use crate::process::{InterruptFrame, ProcessState, SavedGprs, SavedState};
use crate::syscall::user_code_selector;

use super::{SCHEDULER, exec_next_runnable, start_timer};

/// Generates a naked assembly entry point for a preemptable interrupt handler.
///
/// This saves all general-purpose registers before calling the specified handler,
/// allowing the handler to capture the full CPU state for context switching.
///
/// # Safety
/// The generated function is only safe to use as an interrupt handler registered in the IDT.
macro_rules! preemptable_interrupt_entry {
    ($handler:ident) => {{
        #[unsafe(naked)]
        extern "C" fn entry() {
            core::arch::naked_asm!(
                // Save all GPRs (in reverse order so SavedRegsOnStack matches)
                "push rax",
                "push rbx",
                "push rcx",
                "push rdx",
                "push rsi",
                "push rdi",
                "push rbp",
                "push r8",
                "push r9",
                "push r10",
                "push r11",
                "push r12",
                "push r13",
                "push r14",
                "push r15",

                // rdi = pointer to saved regs on stack (first arg)
                "mov rdi, rsp",
                // rsi = pointer to interrupt stack frame (second arg)
                // 15 registers * 8 bytes = 120 bytes offset
                "lea rsi, [rsp + 120]",

                "call {handler}",

                // If handler returns, resume the same process
                // Restore all GPRs
                "pop r15",
                "pop r14",
                "pop r13",
                "pop r12",
                "pop r11",
                "pop r10",
                "pop r9",
                "pop r8",
                "pop rbp",
                "pop rdi",
                "pop rsi",
                "pop rdx",
                "pop rcx",
                "pop rbx",
                "pop rax",

                "iretq",
                handler = sym $handler,
            )
        }
        entry
    }};
}

pub(crate) use preemptable_interrupt_entry;

/// Timer interrupt handler called from the naked entry point.
///
/// This handler is public so the macro can reference it from the parent module.
///
/// Decides whether to preempt the current process and switch to another.
/// If switching, this function does not return - it jumps to exec_next_runnable.
pub(crate) extern "sysv64" fn timer_interrupt_handler(
    saved_gprs: *const SavedGprs,
    interrupt_frame: *const InterruptFrame,
) {
    // Send EOI first to allow other interrupts
    apic::eoi();

    let frame = unsafe { &*interrupt_frame };

    // Only preempt if we interrupted userspace (ring 3).
    // If we interrupted the kernel (e.g., during idle loop or syscall handling),
    // just return without restarting the timer - we'll restart it when we
    // next jump to userspace.
    if frame.cs != user_code_selector() as u64 {
        return;
    }

    // Check if we should preempt (there's another runnable process)
    let should_switch = {
        let scheduler = SCHEDULER.read();
        scheduler.as_ref().map_or(false, |s| s.has_other_runnable())
    };

    if should_switch {
        let gprs = unsafe { &*saved_gprs };
        let state = SavedState::from_interrupt(gprs, frame);

        // Save state and switch to next process (doesn't return)
        unsafe {
            preempt_current(state);
        }
    }

    // Resume same process - restart timer
    start_timer();
}

/// Preempt the current process: save its state and switch to the next runnable.
///
/// # Safety
/// This function does not return. It switches to a different process.
unsafe fn preempt_current(state: SavedState) -> ! {
    {
        let mut scheduler = SCHEDULER.write();
        let scheduler = scheduler
            .as_mut()
            .expect("Scheduler has not been initialized");

        let pid = scheduler.current_process_id();
        let process = scheduler
            .processes
            .get_mut(&pid)
            .expect("Current process not found");

        // Save the full CPU state
        process.save_state(state);

        // Mark as runnable (not running)
        scheduler.change_state(pid, ProcessState::Runnable);
    }
    // Lock dropped

    // Start timer and switch to next process
    start_timer();
    unsafe {
        exec_next_runnable();
    }
}
