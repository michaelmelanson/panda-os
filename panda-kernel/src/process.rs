use alloc::vec::Vec;
use core::{
    arch::asm,
    sync::atomic::{AtomicU64, Ordering},
};

use goblin::elf::{
    Elf,
    program_header::{PT_LOAD, pt_to_str},
};
use log::{info, debug, trace};
use x86_64::{VirtAddr, registers::rflags::RFlags};

use crate::{
    context::Context,
    handle::HandleTable,
    memory::{self, Mapping, MappingBacking, MemoryMappingOptions},
    scheduler::RTC,
};

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

/// Jump to userspace at the given IP and SP. This function never returns.
/// Must be called with no locks held, as it will not return to release them.
///
/// If `saved_state` is Some, all syscall registers will be restored before jumping.
/// This is used when resuming a blocked syscall to re-execute it.
pub unsafe fn exec_userspace(ip: VirtAddr, sp: VirtAddr, saved_state: Option<SavedState>) -> ! {
    let rflags = RFlags::INTERRUPT_FLAG.bits();

    if let Some(state) = saved_state {
        debug!("Resuming syscall at IP={:#x}, SP={:#x}", ip.as_u64(), sp.as_u64());
        // Restore all syscall argument registers for syscall re-execution
        unsafe {
            asm!(
                "mov rsp, {stack_pointer}",
                "swapgs",
                "sysretq",
                in("rcx") ip.as_u64(),
                in("r11") rflags,
                in("rax") state.rax,
                in("rdi") state.rdi,
                in("rsi") state.rsi,
                in("rdx") state.rdx,
                in("r10") state.r10,
                in("r8") state.r8,
                in("r9") state.r9,
                stack_pointer = in(reg) sp.as_u64(),
                options(noreturn)
            );
        }
    } else {
        debug!("Jumping to userspace: IP={:#x}, SP={:#x}", ip.as_u64(), sp.as_u64());
        unsafe {
            asm!(
                "mov rsp, {stack_pointer}",
                "swapgs",
                "sysretq",
                in("rcx") ip.as_u64(),
                in("r11") rflags,
                stack_pointer = in(reg) sp.as_u64(),
                options(noreturn)
            );
        }
    }
}

fn load_elf(elf: &Elf<'_>, file_ptr: *const [u8]) -> Vec<Mapping> {
    let mut mappings = Vec::new();

    for header in &elf.program_headers {
        match header.p_type {
            PT_LOAD => {
                let virt_addr = VirtAddr::new(header.p_vaddr);

                // Align down to page boundary for mapping
                let page_offset = virt_addr.as_u64() & 0xFFF;
                let aligned_virt_addr = virt_addr.align_down(4096u64);
                let aligned_size = header.p_memsz as usize + page_offset as usize;

                let mapping = memory::allocate_and_map(
                    aligned_virt_addr,
                    aligned_size,
                    MemoryMappingOptions {
                        user: true,
                        executable: header.is_executable(),
                        writable: header.is_write(),
                    },
                );

                // Copy ELF data to the mapped region (at the original unaligned address)
                // Temporarily disable write protection to allow kernel writes to read-only pages
                let src_ptr = unsafe { file_ptr.byte_add(header.p_offset as usize) as *const u8 };
                memory::without_write_protection(|| unsafe {
                    core::ptr::copy_nonoverlapping(
                        src_ptr,
                        virt_addr.as_mut_ptr(),
                        header.p_filesz as usize,
                    );
                });

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
    Blocked,
}

pub struct Process {
    id: ProcessId,
    state: ProcessState,
    last_scheduled: RTC,
    context: Context,
    sp: VirtAddr,
    ip: VirtAddr,
    /// Memory mappings for this process (code, data, stack). Dropped on process exit.
    #[allow(dead_code)]
    mappings: Vec<Mapping>,
    handles: HandleTable,
    /// Saved CPU state when process is preempted. Only valid when state is Runnable.
    saved_state: Option<SavedState>,
    /// Heap mapping - demand-paged, resizable. Size represents current brk offset from HEAP_BASE.
    heap: Mapping,
}

impl Process {
    pub fn id(&self) -> ProcessId {
        self.id
    }

    pub fn from_elf_data(context: Context, data: *const [u8]) -> Self {
        let data = unsafe { data.as_ref().unwrap() };
        let elf = Elf::parse(data).expect("failed to parse ELF binary");
        assert_eq!(elf.is_64, true, "32-bit binaries are not supported");

        // Save current page table and switch to the new context's page table
        let saved_page_table = memory::current_page_table_phys();
        unsafe { context.activate(); }

        let mut mappings = load_elf(&elf, data);

        // Allocate stack (64KB should be plenty for userspace programs)
        let stack_base = VirtAddr::new(0xb0000000000);
        let stack_size = 64 * 1024;
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

        // Switch back to the original page table
        unsafe { memory::switch_page_table(saved_page_table); }

        // Create demand-paged heap mapping (initially zero size)
        let heap = Mapping::new(
            VirtAddr::new(panda_abi::HEAP_BASE as u64),
            0,
            MappingBacking::DemandPaged,
        );

        Process {
            id: ProcessId::new(),
            state: ProcessState::Runnable,
            last_scheduled: RTC::zero(),
            context,
            sp: stack_pointer,
            ip: VirtAddr::new(elf.entry),
            mappings,
            handles: HandleTable::new(),
            saved_state: None,
            heap,
        }
    }

    /// Get the IP, SP, and page table address needed for exec.
    /// Used by scheduler to exec after releasing locks.
    pub fn exec_params(&self) -> (VirtAddr, VirtAddr, x86_64::PhysAddr, Option<&SavedState>) {
        (self.ip, self.sp, self.context.page_table_phys(), self.saved_state.as_ref())
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

    pub fn handles(&self) -> &HandleTable {
        &self.handles
    }

    pub fn handles_mut(&mut self) -> &mut HandleTable {
        &mut self.handles
    }

    /// Save the CPU state when preempting this process.
    pub fn save_state(&mut self, state: SavedState) {
        self.saved_state = Some(state);
        // Update IP/SP from saved state for next exec
        self.ip = VirtAddr::new(state.rip);
        self.sp = VirtAddr::new(state.rsp);
    }

    /// Get the saved state, if any.
    pub fn saved_state(&self) -> Option<&SavedState> {
        self.saved_state.as_ref()
    }

    /// Take and clear the saved state.
    pub fn take_saved_state(&mut self) -> Option<SavedState> {
        self.saved_state.take()
    }

    /// Get the current program break (end of heap).
    pub fn brk(&self) -> VirtAddr {
        VirtAddr::new(panda_abi::HEAP_BASE as u64 + self.heap.size() as u64)
    }

    /// Set the program break. Returns the new break on success, or the old break on failure.
    /// The new break must be within the heap region [HEAP_BASE, HEAP_BASE + HEAP_MAX_SIZE).
    /// When shrinking, pages above the new break are unmapped and freed via the Mapping.
    pub fn set_brk(&mut self, new_brk: VirtAddr) -> VirtAddr {
        let heap_base = panda_abi::HEAP_BASE as u64;
        let heap_end = heap_base + panda_abi::HEAP_MAX_SIZE as u64;

        // Validate the new break is within bounds
        if new_brk.as_u64() < heap_base || new_brk.as_u64() > heap_end {
            return self.brk();
        }

        let new_size = (new_brk.as_u64() - heap_base) as usize;
        let old_size = self.heap.size();

        // If shrinking, need to switch to process's page table for unmapping
        if new_size < old_size {
            let saved_pt = memory::current_page_table_phys();
            unsafe { self.context.activate(); }

            self.heap.resize(new_size);

            unsafe { memory::switch_page_table(saved_pt); }
        } else {
            // Growing just updates the size - pages allocated on demand
            self.heap.resize(new_size);
        }

        self.brk()
    }

    /// Get the page table physical address for this process.
    pub fn page_table_phys(&self) -> x86_64::PhysAddr {
        self.context.page_table_phys()
    }
}
