use alloc::{boxed::Box, vec::Vec};
use core::{arch::naked_asm, slice, str};

use log::{debug, error, info};
use spinning_top::Spinlock;

use crate::{context::Context, handle::{HandleId, ResourceType}, process::Process, scheduler, vfs};
use x86_64::{
    VirtAddr,
    instructions::tables::load_tss,
    registers::{
        control::{Efer, EferFlags},
        model_specific::{KernelGsBase, LStar, Star},
        segmentation::{CS, DS, SS, Segment},
    },
    structures::{
        gdt::{Descriptor, GlobalDescriptorTable},
        tss::TaskStateSegment,
    },
};

static GDT: Spinlock<GlobalDescriptorTable> = Spinlock::new(GlobalDescriptorTable::new());
static TSS: Spinlock<TaskStateSegment> = Spinlock::new(TaskStateSegment::new());

#[repr(align(0x1000))]
struct KernelStack {
    inner: [u8; 0x10000], // 64KB kernel stack
}
static KERNEL_STACK: KernelStack = KernelStack { inner: [0; 0x10000] };

static USER_STACK_PTR: usize = 0x0badc0de;

static INTERRUPT_STACK_0: [u8; 1000] = [0; 1000];
static INTERRUPT_STACK_1: [u8; 1000] = [0; 1000];

pub fn init() {
    let mut tss = TSS.lock();
    tss.privilege_stack_table[0] = VirtAddr::new(KERNEL_STACK.inner.as_ptr() as u64);
    tss.privilege_stack_table[1] = VirtAddr::new(KERNEL_STACK.inner.as_ptr() as u64);
    tss.privilege_stack_table[2] = VirtAddr::new(KERNEL_STACK.inner.as_ptr() as u64);
    tss.interrupt_stack_table[0] = VirtAddr::new(INTERRUPT_STACK_0.as_ptr() as u64);
    tss.interrupt_stack_table[1] = VirtAddr::new(INTERRUPT_STACK_1.as_ptr() as u64);
    drop(tss);

    let mut gdt = GDT.lock();
    let kernel_cs = gdt.append(Descriptor::kernel_code_segment());
    let kernel_ds = gdt.append(Descriptor::kernel_data_segment());
    let tss = gdt.append(Descriptor::tss_segment(unsafe { &*TSS.data_ptr() }));
    let user_ds = gdt.append(Descriptor::user_data_segment());
    let user_cs = gdt.append(Descriptor::user_code_segment());
    drop(gdt);

    unsafe {
        (*GDT.data_ptr()).load();
        CS::set_reg(kernel_cs);
        DS::set_reg(kernel_ds);
        SS::set_reg(kernel_ds);
        load_tss(tss);
    }

    Star::write(user_cs, user_ds, kernel_cs, kernel_ds).expect("STAR failed");
    let syscall_entry_ptr = syscall_entry as *const [u8; 10];
    let syscall_entry_addr = syscall_entry_ptr as usize;

    LStar::write(VirtAddr::new(syscall_entry_addr as u64));
    unsafe {
        Efer::update(|efer| {
            efer.insert(EferFlags::SYSTEM_CALL_EXTENSIONS);
        });
    }

    KernelGsBase::write(VirtAddr::new(&USER_STACK_PTR as *const usize as u64));
}

#[unsafe(naked)]
extern "C" fn syscall_entry() {
    naked_asm!(
        "swapgs",
        "mov gs:[0x0], rsp",        // Save user RSP
        "lea rsp, [{kernel_stack}]",
        "add rsp, 0x10000",

        "push rbx",
        "push rbp",
        "push r11",                 // Save RFLAGS (in r11 from syscall)
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        "push rcx",                 // Save return RIP (in rcx from syscall)
        // Stack args for handler: user_rsp, return_rip, syscall_code
        "push gs:[0x0]",            // arg8: user_rsp
        "push rcx",                 // arg7: return_rip
        "push rax",                 // syscall code as 7th argument
        "mov rcx, r10",             // arg3 (sysv64 uses r10 instead of rcx)
        "call {handler}",
        "add rsp, 24",              // pop the 3 stack args
        "pop rcx",                  // Restore return RIP
        // rax now contains the return value from syscall_handler

        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop rbp",
        "pop rbx",

        "mov rsp, gs:[0x0]",
        "swapgs",
        "sysretq",
        handler = sym syscall_handler,
        kernel_stack = sym KERNEL_STACK
    )
}

extern "sysv64" fn syscall_handler(
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    _arg4: usize,
    _arg5: usize,
    code: usize,
    return_rip: usize,
    user_rsp: usize,
) -> isize {
    debug!(
        "SYSCALL: code={code:X}, args: {arg0:X}, {arg1:X}, {arg2:X}, {arg3:X}"
    );

    match code {
        panda_abi::SYSCALL_LOG => {
            let data = unsafe { slice::from_raw_parts(arg0 as *const u8, arg1) };
            let message = match str::from_utf8(data) {
                Ok(message) => message,
                Err(e) => {
                    error!("Invalid log message: {e:?}");
                    return -1;
                }
            };

            info!("LOG: {message}");
            0
        }
        panda_abi::SYSCALL_EXIT => {
            let exit_code = arg0;
            info!("Process exiting with code {exit_code}");

            // If the process exits with non-zero code, fail the test immediately
            if exit_code != 0 {
                crate::qemu::exit_qemu(crate::qemu::QemuExitCode::Failed);
            }

            // Get current process ID and remove it from scheduler
            let current_pid = scheduler::current_process_id();
            info!("Removing process {:?}", current_pid);
            scheduler::remove_process(current_pid);
            info!("Process removed, scheduling next");

            // Schedule next process (does not return)
            unsafe { scheduler::exec_next_runnable(); }
        }
        panda_abi::SYSCALL_OPEN => {
            // arg0 = path pointer, arg1 = path length
            let path = unsafe { slice::from_raw_parts(arg0 as *const u8, arg1) };
            let path = match str::from_utf8(path) {
                Ok(p) => p,
                Err(_) => return -1,
            };

            match vfs::open(path) {
                Some(file) => {
                    let handle = scheduler::with_current_process(|proc| {
                        proc.handles_mut().insert_file(file)
                    });
                    handle as isize
                }
                None => -1,
            }
        }
        panda_abi::SYSCALL_CLOSE => {
            let handle = arg0 as HandleId;
            scheduler::with_current_process(|proc| {
                if proc.handles_mut().remove_typed(handle, ResourceType::File).is_some() {
                    0
                } else {
                    -1
                }
            })
        }
        panda_abi::SYSCALL_READ => {
            let handle = arg0 as HandleId;
            let buf_ptr = arg1 as *mut u8;
            let buf_len = arg2;
            debug!("READ: handle={}, buf_ptr={:#x}, buf_len={}", handle, buf_ptr as usize, buf_len);

            scheduler::with_current_process(|proc| {
                if let Some(file) = proc.handles_mut().get_file_mut(handle) {
                    let buf = unsafe { slice::from_raw_parts_mut(buf_ptr, buf_len) };
                    match file.read(buf) {
                        Ok(n) => n as isize,
                        Err(_) => -1,
                    }
                } else {
                    -1
                }
            })
        }
        panda_abi::SYSCALL_SEEK => {
            let handle = arg0 as HandleId;
            let offset = arg1 as i64;
            let whence = arg2;

            let seek_from = match whence {
                panda_abi::SEEK_SET => vfs::SeekFrom::Start(offset as u64),
                panda_abi::SEEK_CUR => vfs::SeekFrom::Current(offset),
                panda_abi::SEEK_END => vfs::SeekFrom::End(offset),
                _ => return -1,
            };

            scheduler::with_current_process(|proc| {
                if let Some(file) = proc.handles_mut().get_file_mut(handle) {
                    match file.seek(seek_from) {
                        Ok(pos) => pos as isize,
                        Err(_) => -1,
                    }
                } else {
                    -1
                }
            })
        }
        panda_abi::SYSCALL_FSTAT => {
            let handle = arg0 as HandleId;
            let stat_ptr = arg1 as *mut panda_abi::FileStat;

            scheduler::with_current_process(|proc| {
                if let Some(file) = proc.handles_mut().get_file_mut(handle) {
                    let stat = file.stat();
                    unsafe {
                        (*stat_ptr).size = stat.size;
                        (*stat_ptr).is_dir = stat.is_dir;
                    }
                    0
                } else {
                    -1
                }
            })
        }
        panda_abi::SYSCALL_SPAWN => {
            // arg0 = path pointer, arg1 = path length
            let path = unsafe { slice::from_raw_parts(arg0 as *const u8, arg1) };
            let path = match str::from_utf8(path) {
                Ok(p) => p,
                Err(_) => return -1,
            };

            debug!("SPAWN: path={}", path);

            // Open the executable file
            let Some(mut resource) = vfs::open(path) else {
                error!("SPAWN: failed to open {}", path);
                return -1;
            };

            // Get file size and read contents
            let Some(file) = resource.as_file() else {
                error!("SPAWN: {} is not a file", path);
                return -1;
            };

            let stat = file.stat();
            let mut elf_data: Vec<u8> = Vec::new();
            elf_data.resize(stat.size as usize, 0);

            match file.read(&mut elf_data) {
                Ok(n) if n == stat.size as usize => {}
                Ok(n) => {
                    error!("SPAWN: incomplete read: {} of {} bytes", n, stat.size);
                    return -1;
                }
                Err(e) => {
                    error!("SPAWN: failed to read {}: {:?}", path, e);
                    return -1;
                }
            }

            // Create new process from ELF data
            // We leak the Vec to get a stable pointer that outlives this function
            let elf_data = elf_data.into_boxed_slice();
            let elf_ptr: *const [u8] = Box::leak(elf_data);

            let process = Process::from_elf_data(Context::new_user_context(), elf_ptr);
            let pid = process.id();
            debug!("SPAWN: created process {:?}", pid);

            // Add to scheduler
            scheduler::add_process(process);
            // Return the process ID (as a simple integer for now)
            // ProcessId is opaque, but we can return a success indicator
            0
        }
        panda_abi::SYSCALL_YIELD => {
            debug!("YIELD: return_rip={:#x}, user_rsp={:#x}", return_rip, user_rsp);

            // Yield to next process (does not return)
            unsafe {
                scheduler::yield_current(
                    VirtAddr::new(return_rip as u64),
                    VirtAddr::new(user_rsp as u64),
                );
            }
        }
        panda_abi::SYSCALL_SEND => {
            // arg0 = handle, arg1 = operation, arg2-arg5 = operation args
            let handle = arg0 as u32;
            let operation = arg1 as u32;
            handle_send(handle, operation, arg2, arg3, return_rip, user_rsp)
        }
        _ => -1,
    }
}

/// Handle the unified send syscall
fn handle_send(
    handle: u32,
    operation: u32,
    arg0: usize,
    arg1: usize,
    return_rip: usize,
    user_rsp: usize,
) -> isize {
    use panda_abi::*;

    match operation {
        // File operations
        OP_FILE_READ => {
            let buf_ptr = arg0 as *mut u8;
            let buf_len = arg1;
            scheduler::with_current_process(|proc| {
                if let Some(file) = proc.handles_mut().get_file_mut(handle) {
                    let buf = unsafe { slice::from_raw_parts_mut(buf_ptr, buf_len) };
                    match file.read(buf) {
                        Ok(n) => n as isize,
                        Err(_) => -1,
                    }
                } else {
                    -1
                }
            })
        }
        OP_FILE_WRITE => {
            // TODO: File::write not yet implemented in VFS
            -1
        }
        OP_FILE_SEEK => {
            let offset = ((arg1 as u64) << 32 | arg0 as u64) as i64;
            let whence = arg1;
            let seek_from = match whence {
                SEEK_SET => vfs::SeekFrom::Start(offset as u64),
                SEEK_CUR => vfs::SeekFrom::Current(offset),
                SEEK_END => vfs::SeekFrom::End(offset),
                _ => return -1,
            };
            scheduler::with_current_process(|proc| {
                if let Some(file) = proc.handles_mut().get_file_mut(handle) {
                    match file.seek(seek_from) {
                        Ok(pos) => pos as isize,
                        Err(_) => -1,
                    }
                } else {
                    -1
                }
            })
        }
        OP_FILE_STAT => {
            let stat_ptr = arg0 as *mut FileStat;
            scheduler::with_current_process(|proc| {
                if let Some(file) = proc.handles_mut().get_file_mut(handle) {
                    let stat = file.stat();
                    unsafe {
                        (*stat_ptr).size = stat.size;
                        (*stat_ptr).is_dir = stat.is_dir;
                    }
                    0
                } else {
                    -1
                }
            })
        }
        OP_FILE_CLOSE => {
            scheduler::with_current_process(|proc| {
                if proc.handles_mut().remove_typed(handle, ResourceType::File).is_some() {
                    0
                } else {
                    -1
                }
            })
        }

        // Process operations
        OP_PROCESS_YIELD => {
            unsafe {
                scheduler::yield_current(
                    VirtAddr::new(return_rip as u64),
                    VirtAddr::new(user_rsp as u64),
                );
            }
        }
        OP_PROCESS_EXIT => {
            let exit_code = arg0;
            info!("Process exiting with code {exit_code}");
            if exit_code != 0 {
                crate::qemu::exit_qemu(crate::qemu::QemuExitCode::Failed);
            }
            let current_pid = scheduler::current_process_id();
            scheduler::remove_process(current_pid);
            unsafe { scheduler::exec_next_runnable(); }
        }
        OP_PROCESS_GET_PID => {
            // For now, just return 0 for self - we'll implement proper PIDs later
            0
        }
        OP_PROCESS_WAIT => {
            // TODO: Implement wait for child process
            -1
        }
        OP_PROCESS_SIGNAL => {
            // TODO: Implement signals
            -1
        }

        // Environment operations
        OP_ENVIRONMENT_OPEN => {
            let path_ptr = arg0 as *const u8;
            let path_len = arg1;
            let path = unsafe { slice::from_raw_parts(path_ptr, path_len) };
            let path = match str::from_utf8(path) {
                Ok(p) => p,
                Err(_) => return -1,
            };

            match vfs::open(path) {
                Some(file) => {
                    let handle = scheduler::with_current_process(|proc| {
                        proc.handles_mut().insert_file(file)
                    });
                    handle as isize
                }
                None => -1,
            }
        }
        OP_ENVIRONMENT_SPAWN => {
            let path_ptr = arg0 as *const u8;
            let path_len = arg1;
            let path = unsafe { slice::from_raw_parts(path_ptr, path_len) };
            let path = match str::from_utf8(path) {
                Ok(p) => p,
                Err(_) => return -1,
            };

            debug!("SPAWN: path={}", path);

            let Some(mut resource) = vfs::open(path) else {
                error!("SPAWN: failed to open {}", path);
                return -1;
            };

            let Some(file) = resource.as_file() else {
                error!("SPAWN: {} is not a file", path);
                return -1;
            };

            let stat = file.stat();
            let mut elf_data: Vec<u8> = Vec::new();
            elf_data.resize(stat.size as usize, 0);

            match file.read(&mut elf_data) {
                Ok(n) if n == stat.size as usize => {}
                Ok(n) => {
                    error!("SPAWN: incomplete read: {} of {} bytes", n, stat.size);
                    return -1;
                }
                Err(e) => {
                    error!("SPAWN: failed to read {}: {:?}", path, e);
                    return -1;
                }
            }

            let elf_data = elf_data.into_boxed_slice();
            let elf_ptr: *const [u8] = Box::leak(elf_data);

            let process = Process::from_elf_data(Context::new_user_context(), elf_ptr);
            let pid = process.id();
            debug!("SPAWN: created process {:?}", pid);

            scheduler::add_process(process);
            // TODO: Return a process handle instead of 0
            0
        }
        OP_ENVIRONMENT_LOG => {
            let msg_ptr = arg0 as *const u8;
            let msg_len = arg1;
            let msg = unsafe { slice::from_raw_parts(msg_ptr, msg_len) };
            let msg = match str::from_utf8(msg) {
                Ok(m) => m,
                Err(_) => return -1,
            };
            info!("LOG: {msg}");
            0
        }
        OP_ENVIRONMENT_TIME => {
            // TODO: Implement getting time
            0
        }

        _ => {
            error!("Unknown operation: {:#x}", operation);
            -1
        }
    }
}
