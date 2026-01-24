//! Process scheduler with preemptive multitasking and kernel task scheduling.

mod context_switch;
mod rtc;

use core::cmp::Reverse;

use alloc::collections::{BTreeMap, BinaryHeap};
use alloc::sync::Arc;
use alloc::vec::Vec;
use log::{debug, info};
use spinning_top::RwSpinlock;

use crate::apic;
use crate::executor;
use crate::interrupts;
use crate::process::{Process, ProcessId, ProcessState, SavedState, exec_userspace, waker::Waker};

pub use rtc::RTC;

/// Entity that can be scheduled (either a userspace process or kernel task)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SchedulableEntity {
    Process(ProcessId),
    KernelTask(executor::TaskId),
}

pub(crate) static SCHEDULER: RwSpinlock<Option<Scheduler>> = RwSpinlock::new(None);

/// Timer IRQ line (maps to vector 0x20)
const TIMER_IRQ: u8 = 0;

/// Time slice in milliseconds
const TIME_SLICE_MS: u32 = 10;

pub(crate) struct Scheduler {
    processes: BTreeMap<ProcessId, Process>,
    /// Maps each state to a min-heap of (last_scheduled, entity).
    /// Using Reverse<RTC> so that entities with the lowest RTC (least recently scheduled) are picked first.
    /// This provides fair round-robin scheduling for both processes and kernel tasks.
    states: BTreeMap<ProcessState, BinaryHeap<(Reverse<RTC>, SchedulableEntity)>>,
    /// The currently running entity (what prepare_next_runnable selected).
    current: SchedulableEntity,
    /// The last userspace process that actually executed (for syscalls).
    /// Updated only when actually jumping to userspace, not when preparing.
    current_process: ProcessId,
    /// Kernel task last-scheduled times (for fair scheduling)
    kernel_task_rtc: BTreeMap<executor::TaskId, RTC>,
    /// Deadline tracking: maps deadline_ms -> list of tasks to wake
    deadlines: BTreeMap<u64, Vec<executor::TaskId>>,
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

        let entity = SchedulableEntity::Process(pid);

        for state in [
            ProcessState::Runnable,
            ProcessState::Running,
            ProcessState::Blocked,
        ] {
            let state_map = self.states.entry(state).or_default();

            if process.state() == state {
                state_map.push((Reverse(process.last_scheduled()), entity));
            } else {
                // Remove this process from states it doesn't belong to
                state_map.retain(|(_, other_entity)| *other_entity != entity);
            }
        }
    }

    /// Find the next runnable entity (process or kernel task) for execution.
    /// Returns the entity, updating RTC timestamps for fair scheduling.
    pub fn prepare_next_runnable(&mut self) -> Option<SchedulableEntity> {
        // ensure nothing is currently running
        assert!(
            self.states
                .entry(ProcessState::Running)
                .or_default()
                .is_empty()
        );

        let (_, next_entity) = self
            .states
            .entry(ProcessState::Runnable)
            .or_default()
            .pop()?;

        // Update RTC timestamp for fair scheduling
        match next_entity {
            SchedulableEntity::Process(pid) => {
                let Some(process) = self.processes.get_mut(&pid) else {
                    panic!("No process exists with PID {pid:?}");
                };
                process.reset_last_scheduled();
                self.change_state(pid, ProcessState::Running);
            }
            SchedulableEntity::KernelTask(task_id) => {
                // Update kernel task RTC
                self.kernel_task_rtc.insert(task_id, RTC::now());
                // Move to running state
                self.remove_from_state(ProcessState::Runnable, next_entity);
                self.add_to_state(ProcessState::Running, next_entity, RTC::now());
            }
        }

        self.current = next_entity;
        Some(next_entity)
    }

    /// Get execution parameters for a process entity.
    /// Panics if the entity is not a process.
    pub fn get_process_exec_params(
        &self,
        pid: ProcessId,
    ) -> (
        x86_64::VirtAddr,
        x86_64::VirtAddr,
        x86_64::PhysAddr,
        Option<SavedState>,
    ) {
        let Some(process) = self.processes.get(&pid) else {
            panic!("No process exists with PID {pid:?}");
        };
        let (ip, sp, page_table, saved_state) = process.exec_params();
        let saved = saved_state.cloned();
        (ip, sp, page_table, saved)
    }

    /// Remove a process from the scheduler and drop it (releasing resources).
    pub fn remove_process(&mut self, pid: ProcessId) {
        let entity = SchedulableEntity::Process(pid);

        // Remove from state maps
        for state in [
            ProcessState::Runnable,
            ProcessState::Running,
            ProcessState::Blocked,
        ] {
            self.states
                .entry(state)
                .or_default()
                .retain(|(_, other_entity)| *other_entity != entity);
        }

        // Remove and drop the process (this releases all resources)
        self.processes.remove(&pid);
    }

    /// Get the currently running process ID.
    pub fn current_process_id(&self) -> ProcessId {
        self.current_process
    }

    fn change_state(&mut self, pid: ProcessId, state: ProcessState) {
        let Some(process) = self.processes.get_mut(&pid) else {
            panic!("No process exists with PID {pid:?}");
        };

        let entity = SchedulableEntity::Process(pid);
        let prior_state = process.state();
        let last_scheduled = process.last_scheduled();
        process.set_state(state);

        self.remove_from_state(prior_state, entity);
        self.add_to_state(state, entity, last_scheduled);
    }

    fn remove_from_state(&mut self, state: ProcessState, entity: SchedulableEntity) {
        self.state_map(state)
            .retain(|(_, other_entity)| *other_entity != entity);
    }

    fn state_map(
        &mut self,
        state: ProcessState,
    ) -> &mut BinaryHeap<(Reverse<RTC>, SchedulableEntity)> {
        self.states.entry(state).or_default()
    }

    fn add_to_state(
        &mut self,
        state: ProcessState,
        entity: SchedulableEntity,
        last_scheduled: RTC,
    ) {
        self.state_map(state)
            .push((Reverse(last_scheduled), entity));
    }

    fn new(init_process: Process) -> Self {
        let init_pid = init_process.id();
        let mut scheduler = Self {
            processes: Default::default(),
            states: Default::default(),
            current: SchedulableEntity::Process(init_pid),
            current_process: init_pid,
            kernel_task_rtc: Default::default(),
            deadlines: Default::default(),
        };
        scheduler.add(init_process);
        scheduler
    }

    /// Check if there are other runnable entities besides the current one.
    pub(super) fn has_other_runnable(&self) -> bool {
        self.states
            .get(&ProcessState::Runnable)
            .map_or(false, |heap| !heap.is_empty())
    }

    /// Add a kernel task to the scheduler.
    pub fn add_kernel_task(&mut self, task_id: executor::TaskId) {
        let entity = SchedulableEntity::KernelTask(task_id);
        let rtc = RTC::now();
        self.kernel_task_rtc.insert(task_id, rtc);
        self.add_to_state(ProcessState::Runnable, entity, rtc);
        debug!("Added kernel task {:?} to scheduler", task_id);
    }

    /// Change the state of a kernel task.
    pub fn change_kernel_task_state(&mut self, task_id: executor::TaskId, state: ProcessState) {
        let entity = SchedulableEntity::KernelTask(task_id);
        let rtc = self
            .kernel_task_rtc
            .get(&task_id)
            .copied()
            .unwrap_or_else(RTC::now);

        // Remove from all states
        for s in [
            ProcessState::Runnable,
            ProcessState::Running,
            ProcessState::Blocked,
        ] {
            self.remove_from_state(s, entity);
        }

        // Add to new state
        self.add_to_state(state, entity, rtc);
    }

    /// Remove a kernel task from the scheduler.
    pub fn remove_kernel_task(&mut self, task_id: executor::TaskId) {
        let entity = SchedulableEntity::KernelTask(task_id);

        // Remove from all states
        for state in [
            ProcessState::Runnable,
            ProcessState::Running,
            ProcessState::Blocked,
        ] {
            self.remove_from_state(state, entity);
        }

        self.kernel_task_rtc.remove(&task_id);
        debug!("Removed kernel task {:?} from scheduler", task_id);
    }

    /// Register a deadline for a kernel task.
    /// When the deadline arrives, the task will be woken (moved to Runnable state).
    pub fn register_deadline(&mut self, task_id: executor::TaskId, deadline_ms: u64) {
        self.deadlines
            .entry(deadline_ms)
            .or_insert_with(Vec::new)
            .push(task_id);
    }

    /// Wake tasks whose deadlines have arrived.
    /// Returns the number of tasks woken.
    pub fn wake_deadline_tasks(&mut self, now_ms: u64) -> usize {
        let mut tasks_to_wake = Vec::new();
        let mut expired_deadlines = Vec::new();

        // Collect expired deadlines and tasks (BTreeMap is sorted)
        for (&deadline, tasks) in &self.deadlines {
            if deadline <= now_ms {
                tasks_to_wake.extend(tasks.iter().copied());
                expired_deadlines.push(deadline);
            } else {
                break; // BTreeMap is sorted, no more expired deadlines
            }
        }

        // Wake all collected tasks
        for task_id in &tasks_to_wake {
            self.change_kernel_task_state(*task_id, ProcessState::Runnable);
        }

        // Remove expired deadlines
        for deadline in expired_deadlines {
            self.deadlines.remove(&deadline);
        }

        let woken_count = tasks_to_wake.len();
        if woken_count > 0 {
            debug!("Woke {} kernel tasks at deadline {}", woken_count, now_ms);
        }

        woken_count
    }

    /// Get the next deadline time (for timer calculation).
    pub fn next_deadline(&self) -> Option<u64> {
        self.deadlines.keys().next().copied()
    }
}

pub fn init(init_process: Process) {
    // Install naked interrupt handler for preemptive context switching
    use context_switch::{preemptable_interrupt_entry, timer_interrupt_handler};
    let entry = preemptable_interrupt_entry!(timer_interrupt_handler);
    interrupts::set_raw_handler(TIMER_IRQ, entry as *const () as u64);
    debug!("Preemption initialized with {}ms time slice", TIME_SLICE_MS);

    let mut scheduler = SCHEDULER.write();
    assert!(scheduler.is_none(), "scheduler already initialized");
    *scheduler = Some(Scheduler::new(init_process));
}

/// Start the preemption timer. Called before jumping to userspace.
pub(super) fn start_timer() {
    apic::set_timer_oneshot(TIME_SLICE_MS);
}

/// Start the preemption timer with deadline awareness.
///
/// Sets the timer to min(TIME_SLICE_MS, time_until_next_deadline) so we can
/// preempt userspace in time to meet kernel task deadlines.
pub(super) fn start_timer_with_deadline() {
    let timer_duration = {
        let scheduler = SCHEDULER.read();
        let scheduler = scheduler.as_ref().expect("Scheduler not initialized");

        let now = crate::time::uptime_ms();

        if let Some(deadline) = scheduler.next_deadline() {
            // Wake up when deadline arrives or time slice expires, whichever is first
            let time_until_deadline = deadline.saturating_sub(now);
            let duration = time_until_deadline.min(TIME_SLICE_MS as u64).max(1); // At least 1ms
            duration as u32
        } else {
            TIME_SLICE_MS
        }
    };

    apic::set_timer_oneshot(timer_duration);
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
        let (next_entity, has_processes) = {
            let mut scheduler = SCHEDULER.write();
            let scheduler = scheduler
                .as_mut()
                .expect("Scheduler has not been initialized");
            let entity = scheduler.prepare_next_runnable();
            let has_processes = !scheduler.processes.is_empty();
            (entity, has_processes)
        };
        // Lock is now dropped

        match next_entity {
            Some(SchedulableEntity::Process(pid)) => {
                // Update current_process before jumping to userspace
                {
                    let mut scheduler = SCHEDULER.write();
                    let scheduler = scheduler.as_mut().unwrap();
                    scheduler.current_process = pid;
                }

                // Get process execution parameters
                let (ip, sp, page_table, saved_state) = {
                    let scheduler = SCHEDULER.read();
                    let scheduler = scheduler.as_ref().unwrap();
                    scheduler.get_process_exec_params(pid)
                };

                debug!("exec_next_runnable: jumping to userspace (pid={:?})", pid);
                unsafe {
                    crate::memory::switch_page_table(page_table);
                }
                start_timer_with_deadline();
                unsafe { exec_userspace(ip, sp, saved_state) }
            }

            Some(SchedulableEntity::KernelTask(task_id)) => {
                // Poll the kernel task once
                let result = executor::poll_single_task(task_id);

                // Update scheduler based on poll result
                let mut scheduler = SCHEDULER.write();
                let scheduler = scheduler.as_mut().unwrap();

                match result {
                    executor::PollResult::Completed => {
                        // Task done, remove from scheduler
                        scheduler.remove_kernel_task(task_id);
                    }
                    executor::PollResult::Pending => {
                        // Task blocked, mark as blocked
                        scheduler.change_kernel_task_state(task_id, ProcessState::Blocked);
                    }
                    executor::PollResult::NotFound => {
                        // Task was removed, nothing to do
                    }
                }
                // Immediately loop back to pick next entity
                continue;
            }

            None if has_processes => {
                // No runnable entities but userspace processes still exist - idle until interrupt
                // Ensure timer is running for deadline tasks
                start_timer_with_deadline();
                x86_64::instructions::interrupts::enable_and_hlt();
                // An interrupt woke us - loop back to check for runnable entities
                x86_64::instructions::interrupts::disable();
            }
            None => {
                // No runnable entities and no userspace processes - exit
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
    // Disable interrupts to prevent timer from interfering with lock acquisition
    let flags = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::disable();

    let result = {
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
    };

    // Restore interrupt state
    if flags {
        x86_64::instructions::interrupts::enable();
    }

    result
}

/// Suspend the current process with a given state transition, then switch to next runnable.
///
/// This is the common implementation for yield_current and block_current_on.
///
/// # Safety
/// This function does not return to the caller. It switches to a different process.
unsafe fn suspend_current(
    setup: impl FnOnce(&mut Process, ProcessId),
    new_state: ProcessState,
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

        // Let the caller set up process state
        setup(process, pid);

        // Change to the new state
        scheduler.change_state(pid, new_state);
    }
    // Lock dropped

    // Switch to next process
    unsafe {
        exec_next_runnable();
    }
}

/// Yield the current process: save its state and switch to the next runnable.
/// The return_ip and return_sp are where the process should resume.
///
/// # Safety
/// This function does not return to the caller. It switches to a different process.
pub unsafe fn yield_current(return_ip: x86_64::VirtAddr, return_sp: x86_64::VirtAddr) -> ! {
    unsafe {
        suspend_current(
            |process, _| {
                // Save where to resume (only RIP/RSP needed for yield - no register restore)
                process.set_resume_point(return_ip, return_sp);
            },
            ProcessState::Runnable,
        )
    }
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
    unsafe {
        suspend_current(
            |process, pid| {
                // Save where to resume - full state for syscall re-execution
                let mut state = saved_state;
                state.rip = return_ip.as_u64();
                state.rsp = return_sp.as_u64();
                process.save_state(state);

                // Register this process with the waker before blocking
                waker.set_waiting(pid);
            },
            ProcessState::Blocked,
        )
    }
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
