use core::{arch::naked_asm, slice, str};

use log::{debug, error, info};
use spinning_top::Spinlock;

use crate::{handle::HandleId, scheduler, vfs};
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
        "mov gs:[0x0], rsp",
        "lea rsp, [{kernel_stack}]",
        "add rsp, 0x10000",

        "push rbx",
        "push rbp",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        "push rcx",
        "push rax", // syscall code as 7th argument (on stack)
        "mov rcx, r10", // this is the only argument that differs from sysv64
        "call {handler}",
        "add rsp, 8", // pop the syscall code we pushed
        "pop rcx",
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
) -> isize {
    debug!(
        "SYSCALL: code={code:X}, args: {arg0:X}, {arg1:X}, {arg2:X}, {arg3:X}"
    );

    match code {
        libpanda::syscall::SYSCALL_LOG => {
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
        libpanda::syscall::SYSCALL_EXIT => {
            let exit_code = arg0;
            info!("Process exiting with code {exit_code}");

            // Get current process ID and remove it from scheduler
            let current_pid = scheduler::current_process_id();
            info!("Removing process {:?}", current_pid);
            scheduler::remove_process(current_pid);
            info!("Process removed, scheduling next");

            // Schedule next process (does not return)
            unsafe { scheduler::exec_next_runnable(); }
        }
        libpanda::syscall::SYSCALL_OPEN => {
            // arg0 = path pointer, arg1 = path length
            let path = unsafe { slice::from_raw_parts(arg0 as *const u8, arg1) };
            let path = match str::from_utf8(path) {
                Ok(p) => p,
                Err(_) => return -1,
            };

            match vfs::open(path) {
                Some(file) => {
                    let handle = scheduler::with_current_process(|proc| {
                        proc.handles_mut().insert(file)
                    });
                    handle as isize
                }
                None => -1,
            }
        }
        libpanda::syscall::SYSCALL_CLOSE => {
            let handle = arg0 as HandleId;
            scheduler::with_current_process(|proc| {
                proc.handles_mut().remove(handle);
            });
            0
        }
        libpanda::syscall::SYSCALL_READ => {
            let handle = arg0 as HandleId;
            let buf_ptr = arg1 as *mut u8;
            let buf_len = arg2;
            debug!("READ: handle={}, buf_ptr={:#x}, buf_len={}", handle, buf_ptr as usize, buf_len);

            scheduler::with_current_process(|proc| {
                if let Some(resource) = proc.handles_mut().get_mut(handle) {
                    if let Some(file) = resource.as_file() {
                        let buf = unsafe { slice::from_raw_parts_mut(buf_ptr, buf_len) };
                        match file.read(buf) {
                            Ok(n) => n as isize,
                            Err(_) => -1,
                        }
                    } else {
                        -1
                    }
                } else {
                    -1
                }
            })
        }
        libpanda::syscall::SYSCALL_SEEK => {
            let handle = arg0 as HandleId;
            let offset = arg1 as i64;
            let whence = arg2;

            let seek_from = match whence {
                libpanda::syscall::SEEK_SET => vfs::SeekFrom::Start(offset as u64),
                libpanda::syscall::SEEK_CUR => vfs::SeekFrom::Current(offset),
                libpanda::syscall::SEEK_END => vfs::SeekFrom::End(offset),
                _ => return -1,
            };

            scheduler::with_current_process(|proc| {
                if let Some(resource) = proc.handles_mut().get_mut(handle) {
                    if let Some(file) = resource.as_file() {
                        match file.seek(seek_from) {
                            Ok(pos) => pos as isize,
                            Err(_) => -1,
                        }
                    } else {
                        -1
                    }
                } else {
                    -1
                }
            })
        }
        libpanda::syscall::SYSCALL_FSTAT => {
            let handle = arg0 as HandleId;
            let stat_ptr = arg1 as *mut libpanda::syscall::FileStat;

            scheduler::with_current_process(|proc| {
                if let Some(resource) = proc.handles_mut().get_mut(handle) {
                    if let Some(file) = resource.as_file() {
                        let stat = file.stat();
                        unsafe {
                            (*stat_ptr).size = stat.size;
                            (*stat_ptr).is_dir = stat.is_dir;
                        }
                        0
                    } else {
                        -1
                    }
                } else {
                    -1
                }
            })
        }
        _ => -1,
    }
}
