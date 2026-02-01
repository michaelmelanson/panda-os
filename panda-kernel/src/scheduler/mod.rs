//! Process scheduler with preemptive multitasking and kernel task scheduling.

mod context_switch;
mod deadline;
mod rtc;

use core::cmp::Reverse;

use alloc::collections::{BTreeMap, BinaryHeap};
use log::{debug, info};
use spinning_top::RwSpinlock;

use core::task::{Context, Poll};

use crate::apic;
use crate::executor;
use crate::interrupts;
use crate::process::{
    Process, ProcessId, ProcessState, ProcessWaker, SavedState, return_from_deferred_syscall,
    return_from_interrupt, return_from_syscall,
};

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
    /// Deadline tracking for kernel tasks
    deadline_tracker: deadline::DeadlineTracker,
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
    #[allow(dead_code)]
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
    /// Remove a process from the scheduler, returning it for deferred dropping.
    ///
    /// IMPORTANT: The returned Process must be dropped OUTSIDE of any scheduler
    /// lock to avoid deadlocks. When a Process is dropped, its channel handles
    /// are closed, which may call `wake_process()` on peers, requiring the
    /// scheduler lock.
    pub fn remove_process(&mut self, pid: ProcessId) -> Option<Process> {
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

        // Remove and return the process (caller must drop it outside the lock)
        self.processes.remove(&pid)
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
            deadline_tracker: deadline::DeadlineTracker::new(),
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
        self.deadline_tracker.register(task_id, deadline_ms);
    }

    /// Wake tasks whose deadlines have arrived.
    /// Returns the number of tasks woken.
    pub fn wake_deadline_tasks(&mut self, now_ms: u64) -> usize {
        let tasks = self.deadline_tracker.collect_expired(now_ms);
        let count = tasks.len();
        for task_id in tasks {
            self.change_kernel_task_state(task_id, ProcessState::Runnable);
        }
        count
    }

    /// Get the next deadline time (for timer calculation).
    pub fn next_deadline(&self) -> Option<u64> {
        self.deadline_tracker.next_deadline()
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
                // Check if process has a pending async syscall that needs polling
                // IMPORTANT: We must set current_process and drop the scheduler lock
                // BEFORE polling, because the future may call with_current_process.
                let pending_syscall = {
                    let mut scheduler = SCHEDULER.write();
                    let scheduler = scheduler.as_mut().unwrap();
                    // Set current_process so with_current_process works inside the future
                    scheduler.current_process = pid;
                    // Take the pending syscall out so we can poll without holding the lock
                    scheduler
                        .processes
                        .get_mut(&pid)
                        .unwrap()
                        .take_pending_syscall()
                };
                // Lock is now dropped

                let poll_result = if let Some(pending) = pending_syscall {
                    // Create waker outside the lock
                    let waker = ProcessWaker::new(pid).into_waker();
                    let mut cx = Context::from_waker(&waker);

                    // Poll the future without holding the scheduler lock
                    let result = pending.future.lock().as_mut().poll(&mut cx);

                    if result.is_pending() {
                        // Future not ready — put the pending syscall back
                        let mut scheduler = SCHEDULER.write();
                        let process = scheduler.as_mut().unwrap().processes.get_mut(&pid).unwrap();
                        process.set_pending_syscall(pending);
                        Some((Poll::Pending, None))
                    } else {
                        // Future completed — keep callee_saved for the return path
                        Some((result, Some(pending.callee_saved)))
                    }
                } else {
                    None
                };

                match poll_result {
                    Some((Poll::Ready(result), Some(callee_saved))) => {
                        // Future completed - return result to userspace.
                        // Use return_from_deferred_syscall to restore the callee-saved
                        // registers (rbx/rbp/r12-r15) that were captured when the
                        // syscall went Pending, then sysretq with the result in rax.
                        let (ip, sp, page_table) = {
                            let mut scheduler = SCHEDULER.write();
                            let scheduler = scheduler.as_mut().unwrap();
                            let process = scheduler.processes.get_mut(&pid).unwrap();
                            scheduler.current_process = pid;
                            let (ip, sp, page_table, _) = process.exec_params();
                            (ip, sp, page_table)
                        };

                        // Switch page table before copy-out (needed for writeback)
                        unsafe {
                            crate::memory::switch_page_table(page_table);
                        }

                        // Copy out writeback data if present
                        if let Some(wb) = result.writeback {
                            let ua = unsafe { crate::syscall::user_ptr::UserAccess::new() };
                            let _ = ua.write(wb.dst, &wb.data);
                        }

                        debug!(
                            "exec_next_runnable: async syscall completed (pid={:?}, result={}, ip={:#x}, sp={:#x})",
                            pid,
                            result.code,
                            ip.as_u64(),
                            sp.as_u64(),
                        );
                        start_timer_with_deadline();
                        // Return to userspace, restoring callee-saved regs before sysretq
                        unsafe {
                            return_from_deferred_syscall(ip, sp, result.code as u64, &callee_saved)
                        }
                    }
                    Some((Poll::Pending, _)) | Some((Poll::Ready(_), None)) => {
                        // Future not ready - put process back to blocked state
                        {
                            let mut scheduler = SCHEDULER.write();
                            let scheduler = scheduler.as_mut().unwrap();
                            scheduler.change_state(pid, ProcessState::Blocked);
                        }
                        // Loop back to pick another entity
                        continue;
                    }
                    None => {
                        // No pending syscall - normal execution path
                        // Get process execution parameters and yield callee-saved regs
                        let (ip, sp, page_table, saved_state, yield_callee_saved) = {
                            let mut scheduler = SCHEDULER.write();
                            let scheduler = scheduler.as_mut().unwrap();
                            scheduler.current_process = pid;
                            let process = scheduler.processes.get_mut(&pid).unwrap();
                            let saved_state = process.take_saved_state();
                            let yield_cs = process.take_yield_callee_saved();
                            let (ip, sp, pt, _) = process.exec_params();
                            (ip, sp, pt, saved_state, yield_cs)
                        };

                        debug!("exec_next_runnable: jumping to userspace (pid={:?})", pid);
                        unsafe {
                            crate::memory::switch_page_table(page_table);
                        }
                        start_timer_with_deadline();
                        if let Some(state) = saved_state {
                            // Resuming from preemption - restore full state
                            unsafe { return_from_interrupt(&state) }
                        } else if let Some(callee_saved) = yield_callee_saved {
                            // Resuming from yield - restore callee-saved regs via sysretq
                            unsafe { return_from_deferred_syscall(ip, sp, 0, &callee_saved) }
                        } else {
                            // Fresh start - no registers to restore
                            unsafe { return_from_syscall(ip, sp, 0) }
                        }
                    }
                }
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
///
/// The process is dropped AFTER releasing the scheduler lock to avoid deadlocks.
/// Dropping a process may trigger channel close notifications that wake other
/// processes, which requires the scheduler lock.
pub fn remove_process(pid: ProcessId) {
    let process = {
        let mut scheduler = SCHEDULER.write();
        let scheduler = scheduler
            .as_mut()
            .expect("Scheduler has not been initialized");
        scheduler.remove_process(pid)
    };
    // Process is dropped here, outside the lock
    drop(process);
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
pub unsafe fn yield_current(
    return_ip: x86_64::VirtAddr,
    return_sp: x86_64::VirtAddr,
    callee_saved: crate::syscall::CalleeSavedRegs,
) -> ! {
    unsafe {
        suspend_current(
            |process, _| {
                process.set_resume_point(return_ip, return_sp, callee_saved);
            },
            ProcessState::Runnable,
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
