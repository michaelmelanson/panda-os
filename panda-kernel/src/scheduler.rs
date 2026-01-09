//! Process scheduler with preemptive multitasking.

use core::arch::x86_64::_rdtsc;

use alloc::collections::{BTreeMap, BinaryHeap};
use log::{debug, info};
use spinning_top::RwSpinlock;
use x86_64::structures::idt::InterruptStackFrame;

use crate::apic;
use crate::interrupts::{self, IrqHandlerFunc};
use crate::process::{exec_userspace, Process, ProcessId, ProcessState};

static SCHEDULER: RwSpinlock<Option<Scheduler>> = RwSpinlock::new(None);

/// Timer interrupt vector
const TIMER_VECTOR: u8 = 0x20;

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
    states: BTreeMap<ProcessState, BinaryHeap<(RTC, ProcessId)>>,
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

        for state in [ProcessState::Runnable, ProcessState::Running] {
            let state_map = self.states.entry(state).or_default();

            if process.state() == state {
                state_map.push((process.last_scheduled(), pid));
            } else {
                state_map.retain(|(_, other_pid)| pid == *other_pid);
            }
        }
    }

    /// Find the next runnable process and prepare it for execution.
    /// Returns the IP and SP for exec, or None if no runnable processes.
    pub fn prepare_next_runnable(&mut self) -> Option<(x86_64::VirtAddr, x86_64::VirtAddr)> {
        // ensure no processes are currently running
        assert!(
            self.states
                .entry(ProcessState::Running)
                .or_default()
                .is_empty()
        );

        info!("prepare_next_runnable: looking for runnable process");
        let (_, next_pid) = self.states.entry(ProcessState::Runnable).or_default().pop()?;

        info!("prepare_next_runnable: found process {:?}", next_pid);
        self.change_state(next_pid, ProcessState::Running);
        self.current_pid = next_pid;

        let Some(next_process) = self.processes.get_mut(&next_pid) else {
            panic!("No process exists with PID {next_pid:?}");
        };

        info!("prepare_next_runnable: prepared for exec");
        next_process.reset_last_scheduled();
        Some(next_process.exec_params())
    }

    /// Remove a process from the scheduler and drop it (releasing resources).
    pub fn remove_process(&mut self, pid: ProcessId) {
        // Remove from state maps
        for state in [ProcessState::Runnable, ProcessState::Running] {
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
        self.add_to_state(prior_state, pid, last_scheduled);
    }

    fn remove_from_state(&mut self, state: ProcessState, pid: ProcessId) {
        self.state_map(state)
            .retain(|(_, other_pid)| pid == *other_pid);
    }

    fn state_map(&mut self, state: ProcessState) -> &mut BinaryHeap<(RTC, ProcessId)> {
        self.states.entry(state).or_default()
    }

    fn add_to_state(&mut self, state: ProcessState, pid: ProcessId, last_scheduled: RTC) {
        self.state_map(state).push((last_scheduled, pid));
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
    let mut scheduler = SCHEDULER.write();
    assert!(scheduler.is_none(), "scheduler already initialized");
    *scheduler = Some(Scheduler::new(init_process));
}

/// Initialize preemptive scheduling (install timer interrupt handler).
pub fn init_preemption() {
    interrupts::set_irq_handler(TIMER_VECTOR, Some(timer_interrupt_handler as IrqHandlerFunc));
    debug!("Preemption initialized with {}ms time slice", TIME_SLICE_MS);
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

    debug!("Timer interrupt");

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
    let exec_params = {
        let mut scheduler = SCHEDULER.write();
        let scheduler = scheduler
            .as_mut()
            .expect("Scheduler has not been initialized");
        scheduler.prepare_next_runnable()
    };
    // Lock is now dropped

    match exec_params {
        Some((ip, sp)) => {
            info!("exec_next_runnable: jumping to userspace");
            // Start preemption timer before jumping to userspace
            start_timer();
            unsafe { exec_userspace(ip, sp) }
        }
        None => {
            info!("No runnable processes, halting");
            // Exit QEMU if isa-debug-exit device is present (used by tests)
            crate::qemu::exit_qemu(crate::qemu::QemuExitCode::Success);
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
