use core::{
    alloc::Layout,
    arch::asm,
    sync::atomic::{AtomicU64, Ordering},
};

use goblin::elf::{
    Elf,
    program_header::{PT_LOAD, pt_to_str},
};
use log::trace;
use x86_64::{VirtAddr, registers::rflags::RFlags};

use crate::{
    context::Context,
    memory::{self, MemoryMappingOptions, allocate_frame, allocate_physical},
    scheduler::RTC,
};

fn load_elf(elf: &Elf<'_>, file_ptr: *const [u8]) {
    for header in &elf.program_headers {
        match header.p_type {
            PT_LOAD => {
                let phys_addr = allocate_physical(
                    Layout::from_size_align(header.p_memsz as usize, 4096).unwrap(),
                );
                let region_ptr = unsafe { file_ptr.byte_add(header.p_offset as usize) as *mut u8 };
                unsafe {
                    region_ptr.copy_to_nonoverlapping(
                        phys_addr.start_address().as_u64() as *mut u8,
                        header.p_memsz as usize,
                    );
                }

                let virt_addr = VirtAddr::new(header.p_vaddr);

                memory::map(
                    phys_addr.start_address(),
                    virt_addr,
                    header.p_memsz as usize,
                    MemoryMappingOptions {
                        user: true,
                        executable: header.is_executable(),
                        writable: header.is_write(),
                    },
                );
            }

            _ => trace!("Ignoring {} program header", pt_to_str(header.p_type)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProcessId(u64);

impl ProcessId {
    pub fn new() -> Self {
        static NEXT_PROCESS_ID: AtomicU64 = AtomicU64::new(0);
        ProcessId(NEXT_PROCESS_ID.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProcessState {
    Runnable,
    Running,
}

pub struct Process {
    id: ProcessId,
    state: ProcessState,
    last_scheduled: RTC,
    context: Context,
    sp: VirtAddr,
    ip: VirtAddr,
}

impl Process {
    pub fn id(&self) -> ProcessId {
        self.id
    }

    pub fn from_elf_data(context: Context, data: *const [u8]) -> Self {
        let data = unsafe { data.as_ref().unwrap() };
        let elf = Elf::parse(data).expect("failed to parse ELF binary");
        assert_eq!(elf.is_64, true, "32-bit binaries are not supported");

        load_elf(&elf, data);

        let stack_frame = allocate_frame();
        let stack_base = VirtAddr::new(0xb0000000000);
        let stack_pointer = stack_base + stack_frame.size() - 4;

        memory::map(
            stack_frame.start_address(),
            stack_base,
            stack_frame.size() as usize,
            MemoryMappingOptions {
                user: true,
                executable: false,
                writable: true,
            },
        );

        Process {
            id: ProcessId::new(),
            state: ProcessState::Runnable,
            last_scheduled: RTC::zero(),
            context,
            sp: stack_pointer,
            ip: VirtAddr::new(elf.entry),
        }
    }

    pub unsafe fn exec(&self) -> ! {
        let rflags = RFlags::INTERRUPT_FLAG;
        let rflags = rflags.bits();

        unsafe {
            asm!(
                "mov rsp, {stack_pointer}",
                "sysretq",
                in("ecx") self.ip.as_u64(),
                in("r11") rflags,
                stack_pointer = in(reg) self.sp.as_u64()
            );
        }

        panic!("Exec returned");
    }

    pub(crate) fn state(&self) -> ProcessState {
        self.state
    }

    pub(crate) fn last_scheduled(&self) -> RTC {
        self.last_scheduled
    }

    pub fn set_state(&mut self, runnable: ProcessState) {
        self.state = runnable;
    }

    pub fn reset_last_scheduled(&mut self) {
        self.last_scheduled = RTC::now();
    }
}
