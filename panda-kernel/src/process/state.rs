//! CPU register state for context switching.

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
