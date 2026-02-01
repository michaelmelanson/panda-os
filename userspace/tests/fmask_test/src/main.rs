//! Userspace test verifying that FMASK clears the DF (Direction Flag) on syscall entry
//! but restores it on return to userspace.
//!
//! The entire std → syscall → check-DF → cld sequence is done in inline assembly
//! to avoid running any Rust code while DF is set (Rust assumes DF is clear).
//!
//! Note: QEMU TCG does not honour the SFMASK MSR for clearing DF on syscall
//! entry, so the kernel also has an explicit `cld` in the syscall entry path
//! as defence-in-depth.

#![no_std]
#![no_main]

use libpanda::environment;

/// Make a syscall with DF set and return whether DF was preserved on return.
///
/// Does the entire sequence in inline assembly so no Rust-generated code
/// (which assumes DF=0) runs while DF is set.
///
/// The syscall performs an `environment::log` of the given message.
fn syscall_with_df_set(msg: &str) -> bool {
    let df_after: u64;
    unsafe {
        core::arch::asm!(
            // Set DF — this is what we're testing
            "std",
            // Make the syscall: SYSCALL_SEND(handle=ENVIRONMENT, op=LOG, ptr, len, 0, 0)
            "syscall",
            // DF should be restored by sysretq — read it before cld
            "pushfq",
            "pop {df_out}",
            // Clear DF so Rust code is safe again
            "cld",
            // SYSCALL_SEND = 0x30
            in("rax") 0x30_usize,
            // handle = ENVIRONMENT (1)
            in("rdi") 1_usize,
            // operation = OP_ENVIRONMENT_LOG (0x3_0002)
            in("rsi") 0x3_0002_usize,
            // arg0 = msg pointer
            in("rdx") msg.as_ptr() as usize,
            // arg1 = msg length
            in("r10") msg.len(),
            // arg2, arg3 = 0
            in("r8") 0_usize,
            in("r9") 0_usize,
            df_out = out(reg) df_after,
            // syscall clobbers rcx and r11
            out("rcx") _,
            out("r11") _,
        );
    }
    // Check if DF (bit 10) is still set
    df_after & (1 << 10) != 0
}

libpanda::main! {
    environment::log("FMASK test: starting");

    for _i in 0..5 {
        let preserved = syscall_with_df_set("FMASK test: syscall with DF set completed");
        if !preserved {
            environment::log("FMASK test: FAIL — DF was cleared by syscall return");
            return 1;
        }
    }

    environment::log("FMASK test: all syscalls completed successfully");
    0
}
