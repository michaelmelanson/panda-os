use core::arch::asm;

pub const SYSCALL_LOG: usize = 0x10;
pub const SYSCALL_EXIT: usize = 0x11;

#[inline(always)]
fn syscall(
    code: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
) {
    unsafe {
        asm!(
            "syscall",
            in("rax") code,
            in("rdi") arg0,
            in("rsi") arg1,
            in("rdx") arg2,
            in("r10") arg3,
            in("r8") arg4,
            in("r9") arg5,
            out("rcx") _
        );
    }
}

#[inline(always)]
pub fn syscall_log(message: &str) {
    let bytes = message.as_bytes();
    let (data, len) = (bytes.as_ptr(), bytes.len());
    syscall(SYSCALL_LOG, data as usize, len, 0, 0, 0, 0);
}

#[inline(always)]
pub fn syscall_exit(code: usize) -> ! {
    syscall(SYSCALL_EXIT, code, 0, 0, 0, 0, 0);
    loop {
        unsafe {
            asm!("int3", "hlt");
        }
    }
}
