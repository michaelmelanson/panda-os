//! Process scheduler with preemptive multitasking and kernel task scheduling.

mod context_switch;
mod deadline;
mod rtc;

use core::cmp::Reverse;

use alloc::collections::{BTreeMap, BinaryHeap};
use log::{debug, info, warn};
use spinning_top::RwSpinlock;

use core::task::{Context, Poll};

use crate::apic;
use crate::executor;
use crate::interrupts;
use crate::process::{
    Process, ProcessId, ProcessState, ProcessWaker, SavedState, return_from_deferred_syscall,
    return_from_interrupt, return_from_syscall,
};
use crate::syscall::CalleeSavedRegs;
use crate::syscall::user_ptr::SyscallResult;

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

    /// Update the state maps for a process. If the process no longer exists
    /// (e.g., it was removed between scheduling decisions), this is a no-op.
    fn update_process(&mut self, pid: ProcessId) {
        let Some(process) = self.processes.get(&pid) else {
            warn!("update_process: no process with PID {pid:?}, skipping");
            return;
        };

        let entity = SchedulableEntity::Process(pid);
        let current_state = process.state();
        let last_scheduled = process.last_scheduled();

        for state in [
            ProcessState::Runnable,
            ProcessState::Running,
            ProcessState::Blocked,
        ] {
            let state_map = self.states.entry(state).or_default();

            if current_state == state {
                state_map.push((Reverse(last_scheduled), entity));
            } else {
                // Remove this process from states it doesn't belong to
                state_map.retain(|(_, other_entity)| *other_entity != entity);
            }
        }
    }

    /// Find the next runnable entity (process or kernel task) for execution.
    /// Returns the entity, updating RTC timestamps for fair scheduling.
    ///
    /// If a process entity is popped from the runnable queue but is no longer in
    /// the process table (e.g., it was removed between scheduling decisions), it
    /// is silently skipped and the next entity is tried. This avoids panicking
    /// when a process exits concurrently with scheduling.
    pub fn prepare_next_runnable(&mut self) -> Option<SchedulableEntity> {
        // Invariant: nothing should be in Running state when we pick the next
        // entity. This is a genuine scheduler invariant (not user-influenced).
        assert!(
            self.states
                .entry(ProcessState::Running)
                .or_default()
                .is_empty()
        );

        let runnable = self.states.entry(ProcessState::Runnable).or_default();

        // Loop to skip stale process entries whose PIDs have been removed.
        while let Some((_, next_entity)) = runnable.pop() {
            match next_entity {
                SchedulableEntity::Process(pid) => {
                    let Some(process) = self.processes.get_mut(&pid) else {
                        warn!(
                            "prepare_next_runnable: process {pid:?} no longer exists, skipping"
                        );
                        continue;
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
            return Some(next_entity);
        }

        None
    }

    /// Get execution parameters for a process entity.
    /// Returns `None` if the process no longer exists.
    #[allow(dead_code)]
    pub fn get_process_exec_params(
        &self,
        pid: ProcessId,
    ) -> Option<(
        x86_64::VirtAddr,
        x86_64::VirtAddr,
        x86_64::PhysAddr,
        Option<SavedState>,
    )> {
        let process = self.processes.get(&pid)?;
        let (ip, sp, page_table, saved_state) = process.exec_params();
        let saved = saved_state.cloned();
        Some((ip, sp, page_table, saved))
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

    /// Change a process's scheduling state. If the process no longer exists
    /// (e.g., it was removed concurrently), this logs a warning and returns
    /// `false` instead of panicking.
    fn change_state(&mut self, pid: ProcessId, state: ProcessState) -> bool {
        let Some(process) = self.processes.get_mut(&pid) else {
            warn!("change_state: process {pid:?} no longer exists, ignoring state change");
            return false;
        };

        let entity = SchedulableEntity::Process(pid);
        let prior_state = process.state();
        let last_scheduled = process.last_scheduled();
        process.set_state(state);

        self.remove_from_state(prior_state, entity);
        self.add_to_state(state, entity, last_scheduled);
        true
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
        // Invariant: only called after scheduler is initialised during boot.
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
    // Invariant: scheduler must be initialised before processes can be added.
    let scheduler = scheduler
        .as_mut()
        .expect("Scheduler has not been initialized");
    scheduler.add(process);
}

/// Acquire a write lock on the scheduler and pass a mutable reference to `f`.
///
/// The `expect()` guards against calling scheduler functions before `init()`,
/// which is a genuine kernel invariant (not a user-influenced path).
fn with_scheduler_mut<R>(f: impl FnOnce(&mut Scheduler) -> R) -> R {
    let mut guard = SCHEDULER.write();
    let scheduler = guard
        .as_mut()
        .expect("Scheduler has not been initialized");
    f(scheduler)
}

/// Outcome of polling a process's pending async syscall.
enum PendingSyscallOutcome {
    /// The future completed with a result and the callee-saved registers to restore.
    Completed(SyscallResult, CalleeSavedRegs),
    /// The future is not yet ready; the process should be blocked.
    Blocked,
    /// The process had no pending syscall.
    NoPending,
}

/// Take and poll the pending async syscall for `pid`.
///
/// Sets `current_process` so that `with_current_process` works inside the
/// future. The scheduler lock is dropped before polling and re-acquired
/// only when needed (to put the pending syscall back or discard it).
fn poll_pending_syscall(pid: ProcessId) -> Option<PendingSyscallOutcome> {
    // Take the pending syscall out (requires the lock).
    let pending_syscall = with_scheduler_mut(|scheduler| {
        let Some(process) = scheduler.processes.get_mut(&pid) else {
            warn!("poll_pending_syscall: process {pid:?} vanished, skipping");
            return None;
        };
        // Set current_process so with_current_process works inside the future
        scheduler.current_process = pid;
        Some(process.take_pending_syscall())
    });
    // Lock is now dropped.

    let pending_syscall = pending_syscall?; // None ⇒ process gone

    let Some(pending) = pending_syscall else {
        return Some(PendingSyscallOutcome::NoPending);
    };

    // Create waker and poll without holding the scheduler lock.
    let waker = ProcessWaker::new(pid).into_waker();
    let mut cx = Context::from_waker(&waker);
    let result = pending.future.lock().as_mut().poll(&mut cx);

    if result.is_pending() {
        // Put the pending syscall back if the process still exists.
        with_scheduler_mut(|scheduler| {
            if let Some(process) = scheduler.processes.get_mut(&pid) {
                process.set_pending_syscall(pending);
            } else {
                warn!(
                    "poll_pending_syscall: process {pid:?} removed while polling, discarding future"
                );
            }
        });
        Some(PendingSyscallOutcome::Blocked)
    } else if let Poll::Ready(result) = result {
        Some(PendingSyscallOutcome::Completed(result, pending.callee_saved))
    } else {
        // Poll returned Pending but we already handled that branch above;
        // this is unreachable but included for exhaustiveness.
        Some(PendingSyscallOutcome::Blocked)
    }
}

/// Dispatch a completed async syscall result back to userspace.
///
/// Switches the page table, performs any writeback, and jumps to userspace via
/// `return_from_deferred_syscall`. Returns `None` if the process was removed
/// before we could dispatch.
///
/// # Safety
/// This function does not return — it jumps to userspace.
unsafe fn dispatch_completed_syscall(
    pid: ProcessId,
    result: SyscallResult,
    callee_saved: CalleeSavedRegs,
) -> Option<core::convert::Infallible> {
    let exec_params = with_scheduler_mut(|scheduler| {
        let process = scheduler.processes.get_mut(&pid)?;
        scheduler.current_process = pid;
        let (ip, sp, pt, _) = process.exec_params();
        Some((ip, sp, pt))
    });

    let (ip, sp, page_table) = exec_params?;

    unsafe {
        crate::memory::switch_page_table(page_table);
    }

    // Copy out writeback data if present
    if let Some(wb) = result.writeback {
        let ua = unsafe { crate::syscall::user_ptr::UserAccess::new() };
        let _ = ua.write(wb.dst, &wb.data);
    }

    debug!(
        "dispatch_completed_syscall: pid={pid:?}, result={}, ip={:#x}, sp={:#x}",
        result.code,
        ip.as_u64(),
        sp.as_u64(),
    );
    start_timer_with_deadline();
    unsafe { return_from_deferred_syscall(ip, sp, result.code as u64, &callee_saved) }
}

/// Dispatch a process with no pending syscall (normal execution path).
///
/// Reads the process's saved state (preemption, yield, or fresh start) and
/// jumps to userspace via the appropriate return path. Returns `None` if the
/// process was removed before we could dispatch.
///
/// # Safety
/// This function does not return — it jumps to userspace.
unsafe fn dispatch_normal_process(pid: ProcessId) -> Option<core::convert::Infallible> {
    let exec_params = with_scheduler_mut(|scheduler| {
        let process = scheduler.processes.get_mut(&pid)?;
        scheduler.current_process = pid;
        let saved_state = process.take_saved_state();
        let yield_cs = process.take_yield_callee_saved();
        let (ip, sp, pt, _) = process.exec_params();
        Some((ip, sp, pt, saved_state, yield_cs))
    });

    let (ip, sp, page_table, saved_state, yield_callee_saved) = exec_params?;

    debug!("dispatch_normal_process: jumping to userspace (pid={pid:?})");
    unsafe {
        crate::memory::switch_page_table(page_table);
    }
    start_timer_with_deadline();

    if let Some(state) = saved_state {
        // Resuming from preemption — restore full state
        unsafe { return_from_interrupt(&state) }
    } else if let Some(callee_saved) = yield_callee_saved {
        // Resuming from yield — restore callee-saved regs via sysretq
        unsafe { return_from_deferred_syscall(ip, sp, 0, &callee_saved) }
    } else {
        // Fresh start — no registers to restore
        unsafe { return_from_syscall(ip, sp, 0) }
    }
}

/// Handle a kernel task: poll it once and update the scheduler accordingly.
fn dispatch_kernel_task(task_id: executor::TaskId) {
    let result = executor::poll_single_task(task_id);

    with_scheduler_mut(|scheduler| match result {
        executor::PollResult::Completed => scheduler.remove_kernel_task(task_id),
        executor::PollResult::Pending => {
            scheduler.change_kernel_task_state(task_id, ProcessState::Blocked)
        }
        executor::PollResult::NotFound => { /* task was already removed */ }
    });
}

/// Execute the next runnable entity in an infinite scheduling loop.
///
/// This function never returns. It continuously picks the next runnable entity
/// (process or kernel task) and dispatches it. For processes, it handles pending
/// async syscalls, preemption state restoration, and fresh starts.
///
/// # Error handling
///
/// If a process is selected for execution but is no longer in the process table
/// (e.g., it was removed between the scheduling decision and the lookup), the
/// scheduler logs a warning and loops back to pick the next entity. This avoids
/// kernel panics when processes exit concurrently with scheduling decisions.
///
/// Scheduler initialisation `expect()` calls within this function guard against
/// use before `init()` — a genuine kernel invariant, not a user-influenced path.
///
/// # Safety
/// This function does not return. It switches to userspace or loops indefinitely.
pub unsafe fn exec_next_runnable() -> ! {
    loop {
        let (next_entity, has_processes) = with_scheduler_mut(|scheduler| {
            let entity = scheduler.prepare_next_runnable();
            let has_processes = !scheduler.processes.is_empty();
            (entity, has_processes)
        });

        match next_entity {
            Some(SchedulableEntity::Process(pid)) => {
                let outcome = poll_pending_syscall(pid);
                let Some(outcome) = outcome else {
                    // Process vanished — pick another entity.
                    continue;
                };

                match outcome {
                    PendingSyscallOutcome::Completed(result, callee_saved) => {
                        if (unsafe { dispatch_completed_syscall(pid, result, callee_saved) })
                            .is_none()
                        {
                            warn!("exec_next_runnable: process {pid:?} removed before async syscall return");
                            continue;
                        }
                    }
                    PendingSyscallOutcome::Blocked => {
                        // Future not ready — block the process and pick another.
                        with_scheduler_mut(|scheduler| {
                            scheduler.change_state(pid, ProcessState::Blocked);
                        });
                        continue;
                    }
                    PendingSyscallOutcome::NoPending => {
                        if (unsafe { dispatch_normal_process(pid) }).is_none() {
                            warn!("exec_next_runnable: process {pid:?} removed before dispatch");
                            continue;
                        }
                    }
                }
            }

            Some(SchedulableEntity::KernelTask(task_id)) => {
                dispatch_kernel_task(task_id);
                continue;
            }

            None if has_processes => {
                // No runnable entities but userspace processes still exist — idle until interrupt.
                start_timer_with_deadline();
                x86_64::instructions::interrupts::enable_and_hlt();
                // An interrupt woke us — loop back to check for runnable entities.
                x86_64::instructions::interrupts::disable();
            }
            None => {
                // No runnable entities and no userspace processes — exit.
                info!("No processes remaining, halting");
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
        // Invariant: scheduler must be initialised before processes can be removed.
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
    // Invariant: scheduler must be initialised before querying process ID.
    let scheduler = scheduler
        .as_ref()
        .expect("Scheduler has not been initialized");
    scheduler.current_process_id()
}

/// Execute a closure with mutable access to the current process.
///
/// # Error handling
///
/// The `expect()` on the scheduler is a boot invariant — this function is only
/// called after the scheduler is initialised. The `expect()` on the current
/// process lookup is also a kernel invariant: `current_process` is always set
/// to a valid PID before any code path that calls `with_current_process`. If
/// the current process were missing, it would indicate a serious internal bug
/// (not a user-influenced race).
pub fn with_current_process<F, R>(f: F) -> R
where
    F: FnOnce(&mut Process) -> R,
{
    // Disable interrupts to prevent timer from interfering with lock acquisition
    let flags = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::disable();

    let result = {
        let mut scheduler = SCHEDULER.write();
        // Invariant: scheduler must be initialised before calling with_current_process.
        let scheduler = scheduler
            .as_mut()
            .expect("Scheduler has not been initialized");
        let pid = scheduler.current_process_id();
        // Invariant: current_process is always set to a valid PID in exec_next_runnable
        // before dispatching to any code that calls with_current_process.
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
/// # Error handling
///
/// The `expect()` on the current process lookup is a kernel invariant:
/// `suspend_current` is only called from an actively running process's syscall
/// path, so the process must still be in the table. If it were missing, it
/// would indicate a serious internal bug.
///
/// # Safety
/// This function does not return to the caller. It switches to a different process.
unsafe fn suspend_current(
    setup: impl FnOnce(&mut Process, ProcessId),
    new_state: ProcessState,
) -> ! {
    {
        let mut scheduler = SCHEDULER.write();
        // Invariant: scheduler must be initialised before suspending processes.
        let scheduler = scheduler
            .as_mut()
            .expect("Scheduler has not been initialized");

        let pid = scheduler.current_process_id();
        // Invariant: current process is always valid when called from an active
        // syscall path (yield or block). The process cannot have been removed
        // because it is currently executing.
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
///
/// If the process no longer exists (e.g., it was removed while a waker was
/// in flight), this is a no-op. This is expected behaviour and not an error.
pub fn wake_process(pid: ProcessId) {
    let mut scheduler = SCHEDULER.write();
    // Invariant: scheduler must be initialised before waking processes.
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
