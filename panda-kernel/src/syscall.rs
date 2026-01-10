use alloc::{boxed::Box, vec::Vec};
use core::{arch::naked_asm, slice, str};

use log::{debug, error, info};
use spinning_top::Spinlock;

/// Callee-saved registers that must be preserved across syscalls.
/// These are saved by syscall_entry and passed to syscall_handler for use
/// when a process blocks and needs to restore full state on resume.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct CalleeSavedRegs {
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
}

use crate::{context::Context, handle::ResourceType, process::Process, resource, scheduler, vfs};
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

/// User mode code segment selector (ring 3). Set during GDT initialization.
static USER_CS_SELECTOR: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(0);

/// Get the user code segment selector. Must be called after syscall::init().
pub fn user_code_selector() -> u16 {
    USER_CS_SELECTOR.load(core::sync::atomic::Ordering::Relaxed)
}

#[repr(align(0x1000))]
struct KernelStack {
    inner: [u8; 0x10000], // 64KB kernel stack
}

// Syscall handler stack - used by syscall_entry via manual RSP switch
static SYSCALL_STACK: KernelStack = KernelStack {
    inner: [0; 0x10000],
};

// Privilege level transition stack - used by CPU when transitioning ring 3 → ring 0
// This is separate from SYSCALL_STACK so interrupts during syscall handling work correctly
static PRIVILEGE_STACK: KernelStack = KernelStack {
    inner: [0; 0x10000],
};

static USER_STACK_PTR: usize = 0x0badc0de;

const INTERRUPT_STACK_SIZE: usize = 8192; // 8KB per interrupt stack
// IST stacks for specific interrupt handlers (page fault, double fault, etc.)
static INTERRUPT_STACK_0: [u8; INTERRUPT_STACK_SIZE] = [0; INTERRUPT_STACK_SIZE];
static INTERRUPT_STACK_1: [u8; INTERRUPT_STACK_SIZE] = [0; INTERRUPT_STACK_SIZE];

pub fn init() {
    let mut tss = TSS.lock();
    // Privilege stack table entries must point to the TOP of the stack (stacks grow downward)
    // This stack is used by the CPU for ring 3 → ring 0 transitions (interrupts from userspace)
    let privilege_stack_top =
        PRIVILEGE_STACK.inner.as_ptr() as u64 + PRIVILEGE_STACK.inner.len() as u64;
    tss.privilege_stack_table[0] = VirtAddr::new(privilege_stack_top);
    tss.privilege_stack_table[1] = VirtAddr::new(privilege_stack_top);
    tss.privilege_stack_table[2] = VirtAddr::new(privilege_stack_top);
    // IST entries must point to the TOP of the stack (stacks grow downward)
    tss.interrupt_stack_table[0] =
        VirtAddr::new(INTERRUPT_STACK_0.as_ptr() as u64 + INTERRUPT_STACK_SIZE as u64);
    tss.interrupt_stack_table[1] =
        VirtAddr::new(INTERRUPT_STACK_1.as_ptr() as u64 + INTERRUPT_STACK_SIZE as u64);
    drop(tss);

    let mut gdt = GDT.lock();
    let kernel_cs = gdt.append(Descriptor::kernel_code_segment());
    let kernel_ds = gdt.append(Descriptor::kernel_data_segment());
    let tss = gdt.append(Descriptor::tss_segment(unsafe { &*TSS.data_ptr() }));
    let user_ds = gdt.append(Descriptor::user_data_segment());
    let user_cs = gdt.append(Descriptor::user_code_segment());
    drop(gdt);

    // Store user CS selector for use by interrupt handlers
    USER_CS_SELECTOR.store(user_cs.0, core::sync::atomic::Ordering::Relaxed);

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
        kernel_stack = sym SYSCALL_STACK
    )
}

extern "sysv64" fn syscall_handler(
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    code: usize,
    return_rip: usize,
    user_rsp: usize,
    callee_saved: *const CalleeSavedRegs,
) -> isize {
    debug!("SYSCALL: code={code:X}, args: {arg0:X}, {arg1:X}, {arg2:X}, {arg3:X}");

    // Build syscall args for potential restart
    let syscall_args = SyscallArgs {
        code,
        arg0,
        arg1,
        arg2,
        arg3,
        arg4,
        arg5,
    };

    // Read callee-saved registers from the stack
    let callee_saved = unsafe { *callee_saved };

    match code {
        panda_abi::SYSCALL_SEND => {
            // arg0 = handle, arg1 = operation, arg2-arg5 = operation args
            let handle = arg0 as u32;
            let operation = arg1 as u32;
            handle_send(
                handle,
                operation,
                arg2,
                arg3,
                return_rip,
                user_rsp,
                &syscall_args,
                &callee_saved,
            )
        }
        _ => -1,
    }
}

/// Syscall arguments - used to save state for restart after blocking
#[derive(Clone, Copy)]
struct SyscallArgs {
    code: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
}

/// Handle the unified send syscall
fn handle_send(
    handle: u32,
    operation: u32,
    arg0: usize,
    arg1: usize,
    return_rip: usize,
    user_rsp: usize,
    syscall_args: &SyscallArgs,
    callee_saved: &CalleeSavedRegs,
) -> isize {
    use crate::process::SavedState;
    use panda_abi::*;

    match operation {
        // File operations
        OP_FILE_READ => {
            let buf_ptr = arg0 as *mut u8;
            let buf_len = arg1;
            let result = scheduler::with_current_process(|proc| {
                if let Some(file) = proc.handles_mut().get_file_mut(handle) {
                    let buf = unsafe { slice::from_raw_parts_mut(buf_ptr, buf_len) };
                    file.read(buf)
                } else {
                    Err(vfs::FsError::NotFound)
                }
            });
            match result {
                Ok(n) => n as isize,
                Err(vfs::FsError::WouldBlock(waker)) => {
                    // Block the process on this waker.
                    // Save RIP-2 to re-execute the syscall instruction when resumed.
                    // The syscall instruction is 2 bytes (0F 05).
                    // Save all registers needed to re-execute the syscall, including
                    // callee-saved registers which must be preserved across the blocking.
                    let syscall_ip = return_rip - 2;
                    let saved_state = SavedState {
                        // Syscall argument registers
                        rax: syscall_args.code as u64,
                        rdi: syscall_args.arg0 as u64,
                        rsi: syscall_args.arg1 as u64,
                        rdx: syscall_args.arg2 as u64,
                        r10: syscall_args.arg3 as u64,
                        r8: syscall_args.arg4 as u64,
                        r9: syscall_args.arg5 as u64,
                        // Callee-saved registers
                        rbx: callee_saved.rbx,
                        rbp: callee_saved.rbp,
                        r12: callee_saved.r12,
                        r13: callee_saved.r13,
                        r14: callee_saved.r14,
                        r15: callee_saved.r15,
                        ..Default::default()
                    };
                    unsafe {
                        scheduler::block_current_on(
                            waker,
                            VirtAddr::new(syscall_ip as u64),
                            VirtAddr::new(user_rsp as u64),
                            saved_state,
                        );
                    }
                }
                Err(_) => -1,
            }
        }
        OP_FILE_WRITE => {
            let buf_ptr = arg0 as *const u8;
            let buf_len = arg1;
            scheduler::with_current_process(|proc| {
                if let Some(file) = proc.handles_mut().get_file_mut(handle) {
                    let buf = unsafe { slice::from_raw_parts(buf_ptr, buf_len) };
                    match file.write(buf) {
                        Ok(n) => n as isize,
                        Err(_) => -1,
                    }
                } else {
                    -1
                }
            })
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
        OP_FILE_CLOSE => scheduler::with_current_process(|proc| {
            if proc
                .handles_mut()
                .remove_typed(handle, ResourceType::File)
                .is_some()
            {
                0
            } else {
                -1
            }
        }),

        // Process operations
        OP_PROCESS_YIELD => unsafe {
            scheduler::yield_current(
                VirtAddr::new(return_rip as u64),
                VirtAddr::new(user_rsp as u64),
            );
        },
        OP_PROCESS_EXIT => {
            let exit_code = arg0;
            info!("Process exiting with code {exit_code}");
            if exit_code != 0 {
                crate::qemu::exit_qemu(crate::qemu::QemuExitCode::Failed);
            }
            let current_pid = scheduler::current_process_id();
            scheduler::remove_process(current_pid);
            unsafe {
                scheduler::exec_next_runnable();
            }
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
        OP_PROCESS_BRK => {
            let new_brk = arg0;
            debug!("BRK: requested new_brk = {:#x}", new_brk);
            scheduler::with_current_process(|proc| {
                if new_brk == 0 {
                    // Query current break
                    let current = proc.brk().as_u64() as isize;
                    debug!("BRK: query, returning {:#x}", current);
                    current
                } else {
                    // Set new break
                    let result = proc.set_brk(VirtAddr::new(new_brk as u64));
                    debug!("BRK: set, returning {:#x}", result.as_u64());
                    result.as_u64() as isize
                }
            })
        }

        // Environment operations
        OP_ENVIRONMENT_OPEN => {
            let uri_ptr = arg0 as *const u8;
            let uri_len = arg1;
            let uri = unsafe { slice::from_raw_parts(uri_ptr, uri_len) };
            let uri = match str::from_utf8(uri) {
                Ok(u) => u,
                Err(_) => return -1,
            };

            match resource::open(uri) {
                Some(res) => {
                    let handle =
                        scheduler::with_current_process(|proc| proc.handles_mut().insert_file(res));
                    handle as isize
                }
                None => -1,
            }
        }
        OP_ENVIRONMENT_SPAWN => {
            let uri_ptr = arg0 as *const u8;
            let uri_len = arg1;
            let uri = unsafe { slice::from_raw_parts(uri_ptr, uri_len) };
            let uri = match str::from_utf8(uri) {
                Ok(u) => u,
                Err(_) => return -1,
            };

            debug!("SPAWN: uri={}", uri);

            let Some(mut resource) = resource::open(uri) else {
                error!("SPAWN: failed to open {}", uri);
                return -1;
            };

            let Some(file) = resource.as_file() else {
                error!("SPAWN: {} is not a file", uri);
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
                    error!("SPAWN: failed to read {}: {:?}", uri, e);
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
