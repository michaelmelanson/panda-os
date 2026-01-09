use alloc::vec::Vec;
use core::{
    arch::asm,
    sync::atomic::{AtomicU64, Ordering},
};

use goblin::elf::{
    Elf,
    program_header::{PT_LOAD, pt_to_str},
};
use log::{debug, trace};
use x86_64::{VirtAddr, registers::rflags::RFlags};

use crate::{
    context::Context,
    memory::{self, Mapping, MemoryMappingOptions},
    scheduler::RTC,
};

fn load_elf(elf: &Elf<'_>, file_ptr: *const [u8]) -> Vec<Mapping> {
    let mut mappings = Vec::new();

    for header in &elf.program_headers {
        match header.p_type {
            PT_LOAD => {
                let virt_addr = VirtAddr::new(header.p_vaddr);

                let mapping = memory::allocate_and_map(
                    virt_addr,
                    header.p_memsz as usize,
                    MemoryMappingOptions {
                        user: true,
                        executable: header.is_executable(),
                        writable: header.is_write(),
                    },
                );

                // Copy ELF data to the mapped region
                let src_ptr = unsafe { file_ptr.byte_add(header.p_offset as usize) as *const u8 };
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        src_ptr,
                        virt_addr.as_mut_ptr(),
                        header.p_filesz as usize,
                    );
                }

                mappings.push(mapping);
            }

            _ => trace!("Ignoring {} program header", pt_to_str(header.p_type)),
        }
    }

    mappings
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
    mappings: Vec<Mapping>,
}

impl Process {
    pub fn id(&self) -> ProcessId {
        self.id
    }

    pub fn from_elf_data(context: Context, data: *const [u8]) -> Self {
        let data = unsafe { data.as_ref().unwrap() };
        let elf = Elf::parse(data).expect("failed to parse ELF binary");
        assert_eq!(elf.is_64, true, "32-bit binaries are not supported");

        let mut mappings = load_elf(&elf, data);

        // Allocate stack
        let stack_base = VirtAddr::new(0xb0000000000);
        let stack_size = 4096;
        let stack_mapping = memory::allocate_and_map(
            stack_base,
            stack_size,
            MemoryMappingOptions {
                user: true,
                executable: false,
                writable: true,
            },
        );
        let stack_pointer = stack_base + stack_size as u64 - 8; // 8-byte aligned
        mappings.push(stack_mapping);

        Process {
            id: ProcessId::new(),
            state: ProcessState::Runnable,
            last_scheduled: RTC::zero(),
            context,
            sp: stack_pointer,
            ip: VirtAddr::new(elf.entry),
            mappings,
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
