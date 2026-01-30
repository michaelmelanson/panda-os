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
pub use exec::{return_from_deferred_syscall, return_from_interrupt, return_from_syscall};
pub use info::ProcessInfo;
pub use state::{InterruptFrame, SavedGprs, SavedState};
pub use waker::{ProcessWaker, Waker};

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicU64, Ordering};

use goblin::elf::Elf;
use log::error;
use x86_64::VirtAddr;

use crate::handle::HandleTable;
use crate::memory::{self, Mapping, MappingBacking};
use crate::scheduler::RTC;

/// Errors that can occur when creating a process.
#[derive(Debug)]
pub enum ProcessError {
    /// The ELF binary could not be parsed.
    InvalidElf(&'static str),
    /// The binary is 32-bit but only 64-bit is supported.
    Not64Bit,
}

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

/// A pending async syscall that the process is blocked on.
///
/// When a syscall needs to do async I/O, it creates a future and stores it here.
/// The scheduler polls the future when the process is woken. When the future
/// completes, the result is returned to userspace.
pub struct PendingSyscall {
    /// The async operation in progress. Output is the syscall result with optional writeback.
    /// Wrapped in a spinlock to make it Sync (required for Process to be in RwSpinlock).
    /// We require Send because the future may be polled from different contexts.
    pub future: spinning_top::Spinlock<
        Pin<Box<dyn Future<Output = crate::syscall::user_ptr::SyscallResult> + Send>>,
    >,
    /// Callee-saved registers captured when the syscall went Pending.
    /// Must be restored before returning to userspace, since the normal
    /// pop epilogue in syscall_entry is bypassed for deferred syscalls.
    pub callee_saved: crate::syscall::CalleeSavedRegs,
}

impl PendingSyscall {
    /// Create a new pending syscall from a future returning SyscallResult.
    pub fn new(
        future: Pin<Box<dyn Future<Output = crate::syscall::user_ptr::SyscallResult> + Send>>,
        callee_saved: crate::syscall::CalleeSavedRegs,
    ) -> Self {
        Self {
            future: spinning_top::Spinlock::new(future),
            callee_saved,
        }
    }
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
    /// Free buffer virtual address ranges (start_address -> size_in_pages).
    /// Sorted by address for efficient merging of adjacent ranges.
    buffer_free_ranges: BTreeMap<VirtAddr, usize>,
    /// Pending async syscall future. When set, the process is blocked waiting
    /// for this future to complete. The scheduler polls it when the process is woken.
    pending_syscall: Option<PendingSyscall>,
    /// Callee-saved registers captured at yield time. Used by the resume path
    /// to restore rbx/rbp/r12-r15 before sysretq.
    yield_callee_saved: Option<crate::syscall::CalleeSavedRegs>,
}

impl Process {
    pub fn id(&self) -> ProcessId {
        self.id
    }

    /// Create a process from ELF data.
    ///
    /// Returns an error if the ELF binary is malformed or unsupported.
    pub fn from_elf_data(context: Context, data: *const [u8]) -> Result<Self, ProcessError> {
        let data = unsafe { data.as_ref().unwrap() };
        let elf_parsed = match Elf::parse(data) {
            Ok(elf) => elf,
            Err(e) => {
                error!("Failed to parse ELF binary: {}", e);
                return Err(ProcessError::InvalidElf("failed to parse ELF binary"));
            }
        };
        if !elf_parsed.is_64 {
            error!("32-bit binaries are not supported");
            return Err(ProcessError::Not64Bit);
        }

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

        // Initialize the entire buffer region as free
        let mut buffer_free_ranges = BTreeMap::new();
        let buffer_pages = panda_abi::BUFFER_MAX_SIZE / 4096;
        buffer_free_ranges.insert(VirtAddr::new(panda_abi::BUFFER_BASE as u64), buffer_pages);

        // Create handle table with default mailbox at HANDLE_MAILBOX
        let mut handles = HandleTable::new();
        let default_mailbox = crate::resource::Mailbox::new();
        handles.insert_at(panda_abi::HANDLE_MAILBOX, default_mailbox);

        Ok(Process {
            id,
            state: ProcessState::Runnable,
            last_scheduled: RTC::zero(),
            context,
            sp: stack_pointer,
            ip: VirtAddr::new(elf_parsed.entry),
            mappings,
            handles,
            saved_state: None,
            stack,
            heap,
            info: Arc::new(ProcessInfo::new(id)),
            buffer_free_ranges,
            pending_syscall: None,
            yield_callee_saved: None,
        })
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

    /// Set IP/SP and callee-saved registers for resumption (used by yield).
    /// Does NOT set saved_state — callee-saved regs are restored via
    /// `return_from_deferred_syscall` instead.
    pub fn set_resume_point(
        &mut self,
        ip: VirtAddr,
        sp: VirtAddr,
        callee_saved: crate::syscall::CalleeSavedRegs,
    ) {
        self.ip = ip;
        self.sp = sp;
        self.saved_state = None;
        self.yield_callee_saved = Some(callee_saved);
    }

    /// Take and clear the yield callee-saved registers.
    pub fn take_yield_callee_saved(&mut self) -> Option<crate::syscall::CalleeSavedRegs> {
        self.yield_callee_saved.take()
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

    /// Allocate a virtual address range for a buffer.
    /// Uses first-fit allocation from the free list.
    /// Returns None if out of buffer space.
    pub fn alloc_buffer_vaddr(&mut self, num_pages: usize) -> Option<VirtAddr> {
        // Find a suitable free range (first-fit)
        let (&free_addr, &free_pages) = self
            .buffer_free_ranges
            .iter()
            .find(|&(_, &pages)| pages >= num_pages)?;

        // Remove the free range
        self.buffer_free_ranges.remove(&free_addr);

        if free_pages > num_pages {
            // Partial fit, add back the remaining range
            let remaining_addr = VirtAddr::new(free_addr.as_u64() + (num_pages as u64 * 4096));
            let remaining_pages = free_pages - num_pages;
            self.buffer_free_ranges
                .insert(remaining_addr, remaining_pages);
        }

        Some(free_addr)
    }

    /// Free a buffer's virtual address range and add it to the free list.
    /// Adjacent free ranges are merged to reduce fragmentation.
    pub fn free_buffer_vaddr(&mut self, vaddr: VirtAddr, num_pages: usize) {
        let end_addr = VirtAddr::new(vaddr.as_u64() + (num_pages as u64 * 4096));

        // Check if we can merge with the previous range (one that ends where this starts)
        let prev_merge = self.buffer_free_ranges.range(..vaddr).next_back().and_then(
            |(&prev_addr, &prev_pages)| {
                let prev_end = VirtAddr::new(prev_addr.as_u64() + (prev_pages as u64 * 4096));
                if prev_end == vaddr {
                    Some((prev_addr, prev_pages))
                } else {
                    None
                }
            },
        );

        // Check if we can merge with the next range (one that starts where this ends)
        let next_merge =
            self.buffer_free_ranges
                .range(vaddr..)
                .next()
                .and_then(|(&next_addr, &next_pages)| {
                    if next_addr == end_addr {
                        Some((next_addr, next_pages))
                    } else {
                        None
                    }
                });

        match (prev_merge, next_merge) {
            (Some((prev_addr, prev_pages)), Some((next_addr, next_pages))) => {
                // Merge with both previous and next
                self.buffer_free_ranges.remove(&prev_addr);
                self.buffer_free_ranges.remove(&next_addr);
                self.buffer_free_ranges
                    .insert(prev_addr, prev_pages + num_pages + next_pages);
            }
            (Some((prev_addr, prev_pages)), None) => {
                // Merge with previous only
                self.buffer_free_ranges.remove(&prev_addr);
                self.buffer_free_ranges
                    .insert(prev_addr, prev_pages + num_pages);
            }
            (None, Some((next_addr, next_pages))) => {
                // Merge with next only
                self.buffer_free_ranges.remove(&next_addr);
                self.buffer_free_ranges
                    .insert(vaddr, num_pages + next_pages);
            }
            (None, None) => {
                // No merging, just insert
                self.buffer_free_ranges.insert(vaddr, num_pages);
            }
        }
    }

    /// Get the page table physical address for this process.
    pub fn page_table_phys(&self) -> x86_64::PhysAddr {
        self.context.page_table_phys()
    }

    /// Check if the process has a pending async syscall.
    pub fn has_pending_syscall(&self) -> bool {
        self.pending_syscall.is_some()
    }

    /// Set the pending syscall future.
    pub fn set_pending_syscall(&mut self, pending: PendingSyscall) {
        self.pending_syscall = Some(pending);
    }

    /// Take and return the pending syscall, clearing it from the process.
    pub fn take_pending_syscall(&mut self) -> Option<PendingSyscall> {
        self.pending_syscall.take()
    }

    /// Get a mutable reference to the pending syscall future for polling.
    pub fn pending_syscall_mut(&mut self) -> Option<&mut PendingSyscall> {
        self.pending_syscall.as_mut()
    }
}
