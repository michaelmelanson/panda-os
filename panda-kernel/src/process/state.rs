//! CPU register state for context switching.

use x86_64::registers::rflags::RFlags;

use crate::syscall::CalleeSavedRegs;

/// Saved CPU register state for context switching.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct SavedState {
    // General-purpose registers
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    // Instruction and stack pointers
    pub rip: u64,
    pub rsp: u64,
    pub rflags: u64,
}

/// GPRs saved on stack by interrupt entry (matches push order).
#[repr(C)]
pub struct SavedGprs {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
}

/// Interrupt stack frame pushed by CPU.
#[repr(C)]
pub struct InterruptFrame {
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl SavedState {
    /// Create a SavedState for re-executing a syscall after blocking.
    ///
    /// The RIP is set to `syscall_ip` (typically the syscall instruction address),
    /// and all syscall argument registers are restored so the syscall re-executes.
    pub fn for_syscall_restart(
        syscall_ip: u64,
        user_rsp: u64,
        syscall_code: usize,
        args: &[usize; 6],
        callee_saved: &CalleeSavedRegs,
    ) -> Self {
        Self {
            rax: syscall_code as u64,
            rdi: args[0] as u64,
            rsi: args[1] as u64,
            rdx: args[2] as u64,
            r10: args[3] as u64,
            r8: args[4] as u64,
            r9: args[5] as u64,
            rbx: callee_saved.rbx,
            rbp: callee_saved.rbp,
            r12: callee_saved.r12,
            r13: callee_saved.r13,
            r14: callee_saved.r14,
            r15: callee_saved.r15,
            rip: syscall_ip,
            rsp: user_rsp,
            rflags: RFlags::INTERRUPT_FLAG.bits(),
            ..Default::default()
        }
    }

    /// Create a SavedState from an interrupt context (for preemption).
    ///
    /// Captures full register state from the interrupt entry's saved GPRs
    /// and the CPU-pushed interrupt frame.
    pub fn from_interrupt(gprs: &SavedGprs, frame: &InterruptFrame) -> Self {
        Self {
            rax: gprs.rax,
            rbx: gprs.rbx,
            rcx: gprs.rcx,
            rdx: gprs.rdx,
            rsi: gprs.rsi,
            rdi: gprs.rdi,
            rbp: gprs.rbp,
            r8: gprs.r8,
            r9: gprs.r9,
            r10: gprs.r10,
            r11: gprs.r11,
            r12: gprs.r12,
            r13: gprs.r13,
            r14: gprs.r14,
            r15: gprs.r15,
            rip: frame.rip,
            rsp: frame.rsp,
            rflags: frame.rflags,
        }
    }
}
