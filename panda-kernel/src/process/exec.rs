//! Userspace execution and context switching.

use core::arch::{asm, naked_asm};

use log::debug;
use x86_64::{VirtAddr, registers::rflags::RFlags};

use super::SavedState;

/// Return to userspace after a syscall completes.
///
/// Uses `sysretq` which is fast but clobbers RCX (set to RIP) and R11 (set to RFLAGS).
/// This is fine for syscall returns since RCX and R11 are caller-saved scratch registers
/// in the System V ABI.
///
/// # Safety
/// Must be called with no locks held, as this function never returns.
pub unsafe fn return_from_syscall(ip: VirtAddr, sp: VirtAddr, result: u64) -> ! {
    let rflags = RFlags::INTERRUPT_FLAG.bits();

    // Before jumping to userspace, ensure KernelGsBase is set.
    // This is needed because we might be context-switching from another process's
    // syscall handler, where swapgs left KernelGsBase with the old user value.
    use x86_64::registers::model_specific::KernelGsBase;
    let kernel_gs = &crate::syscall::gdt::USER_STACK_PTR as *const usize as u64;
    KernelGsBase::write(x86_64::VirtAddr::new(kernel_gs));

    debug!(
        "Returning to userspace: IP={:#x}, SP={:#x}, result={:#x}",
        ip.as_u64(),
        sp.as_u64(),
        result
    );

    unsafe {
        asm!(
            "mov rsp, {stack_pointer}",
            "sysretq",
            in("rcx") ip.as_u64(),
            in("r11") rflags,
            in("rax") result,
            stack_pointer = in(reg) sp.as_u64(),
            options(noreturn)
        );
    }
}

/// Return to userspace after a deferred syscall completes.
///
/// Like `return_from_syscall`, uses the fast `sysretq` path, but first restores
/// the callee-saved registers (rbx, rbp, r12–r15) from the `CalleeSavedRegs`
/// that were captured when the syscall went `Pending`.
///
/// This is needed because the normal restore path (the `pop` epilogue in
/// `syscall_entry`) is skipped when a process yields mid-syscall — the
/// scheduler resumes the process directly, bypassing that epilogue.
///
/// # Safety
/// Must be called with no locks held, as this function never returns.
pub unsafe fn return_from_deferred_syscall(
    ip: VirtAddr,
    sp: VirtAddr,
    result: u64,
    saved: &crate::syscall::CalleeSavedRegs,
) -> ! {
    let rflags = RFlags::INTERRUPT_FLAG.bits();

    use x86_64::registers::model_specific::KernelGsBase;
    let kernel_gs = &crate::syscall::gdt::USER_STACK_PTR as *const usize as u64;
    KernelGsBase::write(x86_64::VirtAddr::new(kernel_gs));

    debug!(
        "Returning to userspace (deferred): IP={:#x}, SP={:#x}, result={:#x}",
        ip.as_u64(),
        sp.as_u64(),
        result
    );

    unsafe {
        asm!(
            // Restore callee-saved registers from the struct
            "mov rbx, [r15]",       // offset 0x00: rbx
            "mov rbp, [r15 + 8]",   // offset 0x08: rbp
            "mov r12, [r15 + 16]",  // offset 0x10: r12
            "mov r13, [r15 + 24]",  // offset 0x18: r13
            "mov r14, [r15 + 32]",  // offset 0x20: r14
            // r15 itself — load last since we're using it as the base pointer
            "mov r15, [r15 + 40]",  // offset 0x28: r15
            "mov rsp, {stack_pointer}",
            "sysretq",
            in("rcx") ip.as_u64(),
            in("r11") rflags,
            in("rax") result,
            in("r15") saved as *const crate::syscall::CalleeSavedRegs,
            stack_pointer = in(reg) sp.as_u64(),
            options(noreturn)
        );
    }
}

/// Return to userspace after an interrupt (e.g., timer preemption).
///
/// Uses `iretq` which restores the full CPU state including RCX and R11.
/// This is necessary for preemption resume because user code may have been
/// interrupted mid-instruction (e.g., `rep movsq` uses RCX as iteration counter).
///
/// # Safety
/// Must be called with no locks held, as this function never returns.
pub unsafe fn return_from_interrupt(state: &SavedState) -> ! {
    // Before jumping to userspace, ensure KernelGsBase is set.
    use x86_64::registers::model_specific::KernelGsBase;
    let kernel_gs = &crate::syscall::gdt::USER_STACK_PTR as *const usize as u64;
    KernelGsBase::write(x86_64::VirtAddr::new(kernel_gs));

    debug!("Resuming at IP={:#x}, SP={:#x}", state.rip, state.rsp);

    unsafe {
        return_from_interrupt_inner(state);
    }
}

/// Naked helper to restore all registers from SavedState and return to userspace.
///
/// Uses iretq instead of sysretq because sysretq clobbers RCX (uses it for RIP),
/// which breaks resumption of instructions like `rep movsq` that use RCX as a counter.
#[unsafe(naked)]
unsafe extern "sysv64" fn return_from_interrupt_inner(_state: *const SavedState) -> ! {
    // SavedState layout (offsets in bytes):
    //   0x00: rax, 0x08: rbx, 0x10: rcx, 0x18: rdx
    //   0x20: rsi, 0x28: rdi, 0x30: rbp, 0x38: r8
    //   0x40: r9,  0x48: r10, 0x50: r11, 0x58: r12
    //   0x60: r13, 0x68: r14, 0x70: r15, 0x78: rip
    //   0x80: rsp, 0x88: rflags
    naked_asm!(
        // rdi = state pointer, save it to a callee-saved reg temporarily
        "mov r15, rdi",
        // Build iretq stack frame (in reverse order, since stack grows down):
        // [rsp+32] SS
        // [rsp+24] RSP
        // [rsp+16] RFLAGS
        // [rsp+8]  CS
        // [rsp+0]  RIP

        // Push SS (user data segment selector = 0x2b = (5 << 3) | 3)
        "push 0x2b",
        // Push user RSP
        "push [r15 + 0x80]",
        // Push RFLAGS
        "push [r15 + 0x88]",
        // Push CS (user code segment selector = 0x33 = (6 << 3) | 3)
        "push 0x33",
        // Push RIP
        "push [r15 + 0x78]",
        // Now restore all GPRs (including rcx and r11, which sysretq would clobber)
        "mov rax, [r15 + 0x00]",
        "mov rbx, [r15 + 0x08]",
        "mov rcx, [r15 + 0x10]", // Now we can restore the real rcx!
        "mov rdx, [r15 + 0x18]",
        "mov rsi, [r15 + 0x20]",
        // rdi restored later (need r15 first)
        "mov rbp, [r15 + 0x30]",
        "mov r8,  [r15 + 0x38]",
        "mov r9,  [r15 + 0x40]",
        "mov r10, [r15 + 0x48]",
        "mov r11, [r15 + 0x50]", // Now we can restore the real r11!
        "mov r12, [r15 + 0x58]",
        "mov r13, [r15 + 0x60]",
        "mov r14, [r15 + 0x68]",
        // Restore rdi before clobbering r15
        "mov rdi, [r15 + 0x28]",
        // Restore r15 last (we were using it as temp)
        "mov r15, [r15 + 0x70]",
        // Return to userspace - iretq pops RIP, CS, RFLAGS, RSP, SS
        "iretq",
    )
}
