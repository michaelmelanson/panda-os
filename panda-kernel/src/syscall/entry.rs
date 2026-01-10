//! Syscall entry point and MSR configuration.

use core::arch::naked_asm;

use x86_64::{
    VirtAddr,
    registers::{
        control::{Efer, EferFlags},
        model_specific::{KernelGsBase, LStar, Star},
    },
};

use super::{gdt, syscall_handler};

/// Initialize syscall MSRs and entry point.
pub fn init() {
    let kernel_cs = gdt::kernel_code_selector();
    let kernel_ds = gdt::kernel_data_selector();
    let user_cs = gdt::user_cs_selector();
    let user_ds = gdt::user_ds_selector();

    Star::write(user_cs, user_ds, kernel_cs, kernel_ds).expect("STAR failed");

    let syscall_entry_ptr = syscall_entry as *const [u8; 10];
    let syscall_entry_addr = syscall_entry_ptr as usize;
    LStar::write(VirtAddr::new(syscall_entry_addr as u64));

    unsafe {
        Efer::update(|efer| {
            efer.insert(EferFlags::SYSTEM_CALL_EXTENSIONS);
        });
    }

    KernelGsBase::write(VirtAddr::new(&gdt::USER_STACK_PTR as *const usize as u64));
}

/// Naked syscall entry point.
///
/// On entry (from userspace via syscall instruction):
/// - rcx = return RIP
/// - r11 = RFLAGS
/// - rax = syscall code
/// - rdi, rsi, rdx, r10, r8, r9 = syscall arguments (r10 instead of rcx due to syscall clobbering rcx)
///
/// This saves callee-saved registers, switches to kernel stack, and calls syscall_handler.
#[unsafe(naked)]
extern "C" fn syscall_entry() {
    naked_asm!(
        "swapgs",
        "mov gs:[0x0], rsp",        // Save user RSP
        "lea rsp, [{kernel_stack}]",
        "add rsp, 0x10000",

        // Push callee-saved registers in CalleeSavedRegs order (reversed for stack)
        // CalleeSavedRegs: rbx, rbp, r12, r13, r14, r15
        "push r15",
        "push r14",
        "push r13",
        "push r12",
        "push rbp",
        "push rbx",

        "push r11",                 // Save RFLAGS (in r11 from syscall)
        "push rcx",                 // Save return RIP (in rcx from syscall)

        // Stack args for handler: callee_saved_ptr, user_rsp, return_rip, syscall_code
        // Stack at this point (before pushes):
        //   [rsp+0]  = rcx (return RIP)
        //   [rsp+8]  = r11 (RFLAGS)
        //   [rsp+16] = rbx (start of CalleeSavedRegs)
        //
        // sysv64 ABI: args in rdi, rsi, rdx, rcx, r8, r9, then stack (right to left)
        // arg3 should be in rcx, but syscall convention puts it in r10 (rcx has return addr)
        "mov rcx, r10",             // arg3: move from r10 to rcx before we use r10 as temp
        "lea r10, [rsp + 16]",      // r10 = pointer to CalleeSavedRegs on stack
        "push r10",                 // arg9: callee_saved_ptr (rsp -= 8)
        "push gs:[0x0]",            // arg8: user_rsp (rsp -= 8)
        "push [rsp + 16]",          // arg7: return_rip (was at rsp+0, now at rsp+16 after 2 pushes)
        "push rax",                 // arg6: syscall code
        "call {handler}",
        "add rsp, 32",              // pop the 4 stack args

        "pop rcx",                  // Restore return RIP
        "pop r11",                  // Restore RFLAGS

        // Restore callee-saved registers
        "pop rbx",
        "pop rbp",
        "pop r12",
        "pop r13",
        "pop r14",
        "pop r15",

        "mov rsp, gs:[0x0]",
        "swapgs",
        "sysretq",
        handler = sym syscall_handler,
        kernel_stack = sym gdt::SYSCALL_STACK
    )
}
