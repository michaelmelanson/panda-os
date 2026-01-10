//! Userspace execution and context switching.

use core::arch::{asm, naked_asm};

use log::debug;
use x86_64::{VirtAddr, registers::rflags::RFlags};

use super::SavedState;

/// Jump to userspace at the given IP and SP. This function never returns.
/// Must be called with no locks held, as it will not return to release them.
///
/// If `saved_state` is Some, all registers (syscall args + callee-saved) will be restored
/// before jumping. This is used when resuming a blocked syscall to re-execute it.
pub unsafe fn exec_userspace(ip: VirtAddr, sp: VirtAddr, saved_state: Option<SavedState>) -> ! {
    let rflags = RFlags::INTERRUPT_FLAG.bits();

    if let Some(state) = saved_state {
        debug!("Resuming at IP={:#x}, SP={:#x}", state.rip, state.rsp);
        // Use naked helper to restore all registers
        unsafe {
            exec_userspace_with_state(&state);
        }
    } else {
        debug!(
            "Jumping to userspace: IP={:#x}, SP={:#x}",
            ip.as_u64(),
            sp.as_u64()
        );
        unsafe {
            asm!(
                "mov rsp, {stack_pointer}",
                "swapgs",
                "sysretq",
                in("rcx") ip.as_u64(),
                in("r11") rflags,
                in("rax") 0u64,  // Return 0 (success) for yield
                stack_pointer = in(reg) sp.as_u64(),
                options(noreturn)
            );
        }
    }
}

/// Naked helper to restore all registers from SavedState and jump to userspace.
/// Arguments (sysv64 ABI):
///   rdi = pointer to SavedState
///
/// Restores ALL registers from SavedState including rcx, r11, rip, rsp, and rflags.
/// Used for both syscall restart and preemption resume.
#[unsafe(naked)]
unsafe extern "sysv64" fn exec_userspace_with_state(_state: *const SavedState) -> ! {
    // SavedState layout (offsets in bytes):
    //   0x00: rax, 0x08: rbx, 0x10: rcx, 0x18: rdx
    //   0x20: rsi, 0x28: rdi, 0x30: rbp, 0x38: r8
    //   0x40: r9,  0x48: r10, 0x50: r11, 0x58: r12
    //   0x60: r13, 0x68: r14, 0x70: r15, 0x78: rip
    //   0x80: rsp, 0x88: rflags
    naked_asm!(
        // rdi = state pointer, save it to a callee-saved reg temporarily
        "mov r15, rdi",
        // Restore most registers from state
        "mov rax, [r15 + 0x00]",
        "mov rbx, [r15 + 0x08]",
        // rcx restored later (needed for sysretq)
        "mov rdx, [r15 + 0x18]",
        "mov rsi, [r15 + 0x20]",
        // rdi restored later
        "mov rbp, [r15 + 0x30]",
        "mov r8,  [r15 + 0x38]",
        "mov r9,  [r15 + 0x40]",
        "mov r10, [r15 + 0x48]",
        // r11 restored later (needed for sysretq)
        "mov r12, [r15 + 0x58]",
        "mov r13, [r15 + 0x60]",
        "mov r14, [r15 + 0x68]",
        // Set up sysretq: rcx = rip, r11 = rflags
        "mov rcx, [r15 + 0x78]", // rcx = saved rip (return address)
        "mov r11, [r15 + 0x88]", // r11 = saved rflags
        // Push user rsp so we can restore it last
        "push [r15 + 0x80]",
        // Restore rdi (was our temp pointer)
        "mov rdi, [r15 + 0x28]",
        // Restore r15 (we were using it as temp)
        "mov r15, [r15 + 0x70]",
        // Set user stack pointer and return
        "pop rsp",
        "swapgs",
        "sysretq",
    )
}
