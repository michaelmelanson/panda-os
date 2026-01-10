//! Process management.
//!
//! This module contains all process-related functionality:
//! - Process struct and lifecycle management
//! - CPU state saving/restoring
//! - ELF loading
//! - Process info for inter-process communication

pub mod context;
mod elf;
mod exec;
pub mod info;
mod state;
pub mod waker;

pub use context::Context;
pub use exec::exec_userspace;
pub use info::ProcessInfo;
pub use state::{InterruptFrame, SavedGprs, SavedState};
pub use waker::Waker;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use goblin::elf::Elf;
use x86_64::VirtAddr;

use crate::handle::HandleTable;
use crate::memory::{self, Mapping, MappingBacking};
use crate::scheduler::RTC;

/// Unique process identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProcessId(u64);

impl ProcessId {
    pub fn new() -> Self {
        static NEXT_PROCESS_ID: AtomicU64 = AtomicU64::new(0);
        ProcessId(NEXT_PROCESS_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// Process execution state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProcessState {
    Runnable,
    Running,
    Blocked,
}

/// A userspace process.
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
    /// Stack mapping - demand-paged. Grows downward from top of region.
    /// Field is kept for RAII cleanup when process exits.
    #[allow(dead_code)]
    stack: Mapping,
    /// Heap mapping - demand-paged, resizable. Size represents current brk offset from HEAP_BASE.
    heap: Mapping,
    /// External process info visible to handle holders.
    /// Survives process exit until all handles are dropped.
    info: Arc<ProcessInfo>,
}

impl Process {
    pub fn id(&self) -> ProcessId {
        self.id
    }

    /// Create a process from ELF data.
    pub fn from_elf_data(context: Context, data: *const [u8]) -> Self {
        let data = unsafe { data.as_ref().unwrap() };
        let elf_parsed = Elf::parse(data).expect("failed to parse ELF binary");
        assert!(elf_parsed.is_64, "32-bit binaries are not supported");

        // Save current page table and switch to the new context's page table
        let saved_page_table = memory::current_page_table_phys();
        unsafe {
            context.activate();
        }

        let mappings = elf::load_elf(&elf_parsed, data);

        // Switch back to the original page table
        unsafe {
            memory::switch_page_table(saved_page_table);
        }

        // Create demand-paged stack mapping (grows downward, pages allocated on fault)
        let stack = Mapping::new(
            VirtAddr::new(panda_abi::STACK_BASE as u64),
            panda_abi::STACK_MAX_SIZE,
            MappingBacking::DemandPaged,
        );
        // Stack pointer starts at top of stack region, 8-byte aligned
        let stack_pointer =
            VirtAddr::new((panda_abi::STACK_BASE + panda_abi::STACK_MAX_SIZE - 8) as u64);

        // Create demand-paged heap mapping (initially zero size)
        let heap = Mapping::new(
            VirtAddr::new(panda_abi::HEAP_BASE as u64),
            0,
            MappingBacking::DemandPaged,
        );

        let id = ProcessId::new();
        Process {
            id,
            state: ProcessState::Runnable,
            last_scheduled: RTC::zero(),
            context,
            sp: stack_pointer,
            ip: VirtAddr::new(elf_parsed.entry),
            mappings,
            handles: HandleTable::new(),
            saved_state: None,
            stack,
            heap,
            info: Arc::new(ProcessInfo::new(id)),
        }
    }

    /// Get the process info (for creating handles).
    pub fn info(&self) -> &Arc<ProcessInfo> {
        &self.info
    }

    /// Set the exit code. Called when process terminates.
    pub fn set_exit_code(&self, code: i32) {
        self.info.set_exit_code(code);
    }

    /// Get the IP, SP, and page table address needed for exec.
    /// Used by scheduler to exec after releasing locks.
    pub fn exec_params(&self) -> (VirtAddr, VirtAddr, x86_64::PhysAddr, Option<&SavedState>) {
        (
            self.ip,
            self.sp,
            self.context.page_table_phys(),
            self.saved_state.as_ref(),
        )
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

    /// Save the CPU state when blocking this process on a syscall.
    /// The saved state will be used to restore registers when resuming.
    pub fn save_state(&mut self, state: SavedState) {
        self.saved_state = Some(state);
        // Update IP/SP from saved state for next exec
        self.ip = VirtAddr::new(state.rip);
        self.sp = VirtAddr::new(state.rsp);
    }

    /// Set only the IP/SP for resumption (used by yield).
    /// Does NOT set saved_state, so registers won't be restored.
    pub fn set_resume_point(&mut self, ip: VirtAddr, sp: VirtAddr) {
        self.ip = ip;
        self.sp = sp;
        self.saved_state = None;
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
    ///
    /// Note: This is called from syscall context where the process's page table is already active,
    /// so no page table switch is needed.
    pub fn set_brk(&mut self, new_brk: VirtAddr) -> VirtAddr {
        let heap_base = panda_abi::HEAP_BASE as u64;
        let heap_end = heap_base + panda_abi::HEAP_MAX_SIZE as u64;

        // Validate the new break is within bounds
        if new_brk.as_u64() < heap_base || new_brk.as_u64() > heap_end {
            return self.brk();
        }

        let new_size = (new_brk.as_u64() - heap_base) as usize;

        // Resize the heap - when shrinking, this unmaps and frees pages above new_size.
        // When growing, pages are allocated on demand via page faults.
        self.heap.resize(new_size);

        self.brk()
    }

    /// Get the page table physical address for this process.
    pub fn page_table_phys(&self) -> x86_64::PhysAddr {
        self.context.page_table_phys()
    }
}
