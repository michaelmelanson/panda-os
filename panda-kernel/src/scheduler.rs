//! Process scheduler with preemptive multitasking.

use core::arch::x86_64::_rdtsc;

use core::cmp::Reverse;

use alloc::collections::{BTreeMap, BinaryHeap};
use log::{debug, info};
use spinning_top::RwSpinlock;
use x86_64::structures::idt::InterruptStackFrame;

use alloc::sync::Arc;

use crate::apic;
use crate::interrupts::{self, IrqHandlerFunc};
use crate::process::{exec_userspace, Process, ProcessId, ProcessState, SavedState};
use crate::waker::Waker;

static SCHEDULER: RwSpinlock<Option<Scheduler>> = RwSpinlock::new(None);

/// Timer IRQ line (maps to vector 0x20)
const TIMER_IRQ: u8 = 0;

/// Time slice in milliseconds
const TIME_SLICE_MS: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RTC(u64);
impl RTC {
    // represents "never" as in "this process has never been scheduled"
    pub fn zero() -> RTC {
        RTC(0)
    }

    pub fn now() -> RTC {
        let timestamp = unsafe { _rdtsc() };
        RTC(timestamp)
    }
}

struct Scheduler {
    processes: BTreeMap<ProcessId, Process>,
    /// Maps each process state to a min-heap of (last_scheduled, pid).
    /// Using Reverse<RTC> so that processes with the lowest RTC (least recently scheduled) are picked first.
    states: BTreeMap<ProcessState, BinaryHeap<(Reverse<RTC>, ProcessId)>>,
    /// The currently running process. Only valid after exec_next_runnable() is called.
    current_pid: ProcessId,
}

impl Scheduler {
    pub fn add(&mut self, process: Process) {
        let id = process.id();
        self.processes.insert(id, process);
        self.update_process(id);
    }

    fn update_process(&mut self, pid: ProcessId) {
        let Some(process) = self.processes.get(&pid) else {
            panic!("No process with PID {pid:?}");
        };

        for state in [ProcessState::Runnable, ProcessState::Running, ProcessState::Blocked] {
            let state_map = self.states.entry(state).or_default();

            if process.state() == state {
                state_map.push((Reverse(process.last_scheduled()), pid));
            } else {
                // Remove this process from states it doesn't belong to
                state_map.retain(|(_, other_pid)| *other_pid != pid);
            }
        }
    }

    /// Find the next runnable process and prepare it for execution.
    /// Returns the IP, SP, page table address, and optional saved state for exec.
    /// The saved state is used when resuming from a blocked syscall to re-execute it.
    pub fn prepare_next_runnable(&mut self) -> Option<(x86_64::VirtAddr, x86_64::VirtAddr, x86_64::PhysAddr, Option<SavedState>)> {
        // ensure no processes are currently running
        assert!(
            self.states
                .entry(ProcessState::Running)
                .or_default()
                .is_empty()
        );

        let (_, next_pid) = self.states.entry(ProcessState::Runnable).or_default().pop()?;

        self.change_state(next_pid, ProcessState::Running);
        self.current_pid = next_pid;

        let Some(next_process) = self.processes.get_mut(&next_pid) else {
            panic!("No process exists with PID {next_pid:?}");
        };

        next_process.reset_last_scheduled();
        let (ip, sp, page_table, saved_state) = next_process.exec_params();
        // Clone the saved state if present
        let saved = saved_state.cloned();
        Some((ip, sp, page_table, saved))
    }

    /// Remove a process from the scheduler and drop it (releasing resources).
    pub fn remove_process(&mut self, pid: ProcessId) {
        // Remove from state maps
        for state in [ProcessState::Runnable, ProcessState::Running, ProcessState::Blocked] {
            self.states
                .entry(state)
                .or_default()
                .retain(|(_, other_pid)| *other_pid != pid);
        }

        // Remove and drop the process (this releases all resources)
        self.processes.remove(&pid);
    }

    /// Get the currently running process ID.
    pub fn current_process_id(&self) -> ProcessId {
        self.current_pid
    }

    fn change_state(&mut self, pid: ProcessId, state: ProcessState) {
        let Some(process) = self.processes.get_mut(&pid) else {
            panic!("No process exists with PID {pid:?}");
        };

        let prior_state = process.state();
        let last_scheduled = process.last_scheduled();
        process.set_state(state);

        self.remove_from_state(prior_state, pid);
        self.add_to_state(state, pid, last_scheduled);
    }

    fn remove_from_state(&mut self, state: ProcessState, pid: ProcessId) {
        self.state_map(state)
            .retain(|(_, other_pid)| *other_pid != pid);
    }

    fn state_map(&mut self, state: ProcessState) -> &mut BinaryHeap<(Reverse<RTC>, ProcessId)> {
        self.states.entry(state).or_default()
    }

    fn add_to_state(&mut self, state: ProcessState, pid: ProcessId, last_scheduled: RTC) {
        self.state_map(state).push((Reverse(last_scheduled), pid));
    }

    fn new(init_process: Process) -> Self {
        let init_pid = init_process.id();
        let mut scheduler = Self {
            processes: Default::default(),
            states: Default::default(),
            current_pid: init_pid,
        };
        scheduler.add(init_process);
        scheduler
    }
}

pub fn init(init_process: Process) {
    // Install timer interrupt handler for preemption
    interrupts::set_irq_handler(TIMER_IRQ, Some(timer_interrupt_handler as IrqHandlerFunc));
    debug!("Preemption initialized with {}ms time slice", TIME_SLICE_MS);

    let mut scheduler = SCHEDULER.write();
    assert!(scheduler.is_none(), "scheduler already initialized");
    *scheduler = Some(Scheduler::new(init_process));
}

/// Start the preemption timer. Called before jumping to userspace.
fn start_timer() {
    apic::set_timer_oneshot(TIME_SLICE_MS);
}

/// Timer interrupt handler for preemption.
///
/// When this fires, we need to save the interrupted process's state and switch
/// to the next runnable process. The interrupt stack frame contains RIP, CS,
/// RFLAGS, RSP, SS - but we also need to save general-purpose registers.
///
/// For now, with only one process, we just restart the timer and return.
/// Full context switching will be implemented when we have multiple processes.
extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // Send EOI first
    apic::eoi();

    // For now, just restart the timer - with one process there's nothing to switch to
    // TODO: Implement full context switch when we have multiple processes
    start_timer();
}

pub fn add_process(process: Process) {
    let mut scheduler = SCHEDULER.write();
    let scheduler = scheduler
        .as_mut()
        .expect("Scheduler has not been initialized");
    scheduler.add(process);
}

pub unsafe fn exec_next_runnable() -> ! {
    loop {
        let (exec_params, has_blocked) = {
            let mut scheduler = SCHEDULER.write();
            let scheduler = scheduler
                .as_mut()
                .expect("Scheduler has not been initialized");
            let params = scheduler.prepare_next_runnable();
            let blocked_count = scheduler
                .states
                .get(&ProcessState::Blocked)
                .map(|h| h.len())
                .unwrap_or(0);
            (params, blocked_count > 0)
        };
        // Lock is now dropped

        match exec_params {
            Some((ip, sp, page_table, saved_state)) => {
                debug!("exec_next_runnable: jumping to userspace");
                // Switch to the process's page table
                unsafe { crate::memory::switch_page_table(page_table); }
                // Start preemption timer before jumping to userspace
                start_timer();
                unsafe { exec_userspace(ip, sp, saved_state) }
            }
            None if has_blocked => {
                // No runnable processes but some are blocked - idle until interrupt
                x86_64::instructions::interrupts::enable_and_hlt();
                // An interrupt woke us - loop back to check for runnable processes
                x86_64::instructions::interrupts::disable();
            }
            None => {
                info!("No processes remaining, halting");
                // Exit QEMU if isa-debug-exit device is present (used by tests)
                crate::qemu::exit_qemu(crate::qemu::QemuExitCode::Success);
            }
        }
    }
}

/// Remove a process from the scheduler and drop it (releasing resources).
pub fn remove_process(pid: ProcessId) {
    let mut scheduler = SCHEDULER.write();
    let scheduler = scheduler
        .as_mut()
        .expect("Scheduler has not been initialized");
    scheduler.remove_process(pid);
}

/// Get the currently running process ID.
pub fn current_process_id() -> ProcessId {
    let scheduler = SCHEDULER.read();
    let scheduler = scheduler
        .as_ref()
        .expect("Scheduler has not been initialized");
    scheduler.current_process_id()
}

/// Execute a closure with mutable access to the current process.
pub fn with_current_process<F, R>(f: F) -> R
where
    F: FnOnce(&mut Process) -> R,
{
    let mut scheduler = SCHEDULER.write();
    let scheduler = scheduler
        .as_mut()
        .expect("Scheduler has not been initialized");
    let pid = scheduler.current_process_id();
    let process = scheduler
        .processes
        .get_mut(&pid)
        .expect("Current process not found");
    f(process)
}

/// Yield the current process: save its state and switch to the next runnable.
/// The return_ip and return_sp are where the process should resume.
///
/// # Safety
/// This function does not return to the caller. It switches to a different process.
pub unsafe fn yield_current(return_ip: x86_64::VirtAddr, return_sp: x86_64::VirtAddr) -> ! {
    {
        let mut scheduler = SCHEDULER.write();
        let scheduler = scheduler
            .as_mut()
            .expect("Scheduler has not been initialized");

        let pid = scheduler.current_process_id();
        let process = scheduler
            .processes
            .get_mut(&pid)
            .expect("Current process not found");

        // Save where to resume (only RIP/RSP needed for yield - no register restore)
        process.set_resume_point(return_ip, return_sp);

        // Mark as runnable (not running)
        scheduler.change_state(pid, ProcessState::Runnable);
    }
    // Lock dropped

    // Switch to next process
    unsafe { exec_next_runnable(); }
}

/// Block the current process on a waker: save its state and switch to the next runnable.
/// The process will be woken when the waker's `wake()` method is called.
///
/// The saved state includes all registers needed to re-execute the syscall when resumed.
/// RIP should point to the syscall instruction, and all argument registers are preserved.
///
/// # Safety
/// This function does not return to the caller. It switches to a different process.
pub unsafe fn block_current_on(
    waker: Arc<Waker>,
    return_ip: x86_64::VirtAddr,
    return_sp: x86_64::VirtAddr,
    saved_state: SavedState,
) -> ! {
    {
        let mut scheduler = SCHEDULER.write();
        let scheduler = scheduler
            .as_mut()
            .expect("Scheduler has not been initialized");

        let pid = scheduler.current_process_id();
        let process = scheduler
            .processes
            .get_mut(&pid)
            .expect("Current process not found");

        // Save where to resume - full state for syscall re-execution
        let mut state = saved_state;
        state.rip = return_ip.as_u64();
        state.rsp = return_sp.as_u64();
        process.save_state(state);

        // Register this process with the waker before blocking
        waker.set_waiting(pid);

        // Mark as blocked
        scheduler.change_state(pid, ProcessState::Blocked);
    }
    // Lock dropped

    // Switch to next process
    unsafe { exec_next_runnable(); }
}

/// Wake a blocked process, making it runnable again.
/// Called by wakers when data becomes available.
pub fn wake_process(pid: ProcessId) {
    let mut scheduler = SCHEDULER.write();
    let scheduler = scheduler
        .as_mut()
        .expect("Scheduler has not been initialized");

    // Only wake if the process exists and is blocked
    if let Some(process) = scheduler.processes.get(&pid) {
        if process.state() == ProcessState::Blocked {
            scheduler.change_state(pid, ProcessState::Runnable);
            debug!("Woke process {:?}", pid);
        }
    }
}
