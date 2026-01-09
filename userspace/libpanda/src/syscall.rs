use core::arch::asm;

// Re-export ABI constants
pub use panda_abi::{
    FileStat, SEEK_CUR, SEEK_END, SEEK_SET, SYSCALL_CLOSE, SYSCALL_EXIT, SYSCALL_FSTAT,
    SYSCALL_LOG, SYSCALL_OPEN, SYSCALL_READ, SYSCALL_SEEK,
};

#[inline(always)]
fn syscall(
    code: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
) -> isize {
    let result: isize;
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
            lateout("rax") result,
            out("rcx") _,
            out("r11") _,
        );
    }
    result
}

#[inline(always)]
pub fn syscall_log(message: &str) {
    let bytes = message.as_bytes();
    let (data, len) = (bytes.as_ptr(), bytes.len());
    let _ = syscall(SYSCALL_LOG, data as usize, len, 0, 0, 0, 0);
}

#[inline(always)]
pub fn syscall_exit(code: usize) -> ! {
    let _ = syscall(SYSCALL_EXIT, code, 0, 0, 0, 0, 0);
    loop {
        unsafe {
            asm!("int3", "hlt");
        }
    }
}

/// Open a file, returning a handle or -1 on error
#[inline(always)]
pub fn open(path: &str) -> isize {
    let bytes = path.as_bytes();
    syscall(SYSCALL_OPEN, bytes.as_ptr() as usize, bytes.len(), 0, 0, 0, 0)
}

/// Close a handle
#[inline(always)]
pub fn close(handle: u32) {
    let _ = syscall(SYSCALL_CLOSE, handle as usize, 0, 0, 0, 0, 0);
}

/// Read from a handle into a buffer, returning bytes read or -1 on error
#[inline(always)]
pub fn read(handle: u32, buf: &mut [u8]) -> isize {
    syscall(
        SYSCALL_READ,
        handle as usize,
        buf.as_mut_ptr() as usize,
        buf.len(),
        0,
        0,
        0,
    )
}

/// Seek within a file, returning new position or -1 on error
#[inline(always)]
pub fn seek(handle: u32, offset: i64, whence: usize) -> isize {
    syscall(
        SYSCALL_SEEK,
        handle as usize,
        offset as usize,
        whence,
        0,
        0,
        0,
    )
}

/// Get file stats by handle, returning 0 on success or -1 on error
#[inline(always)]
pub fn fstat(handle: u32, stat: &mut FileStat) -> isize {
    syscall(
        SYSCALL_FSTAT,
        handle as usize,
        stat as *mut FileStat as usize,
        0,
        0,
        0,
        0,
    )
}
