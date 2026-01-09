use core::arch::x86_64::_rdtsc;

use alloc::collections::{BTreeMap, BinaryHeap};
use spinning_top::RwSpinlock;

use crate::process::{Process, ProcessId, ProcessState};

static SCHEDULER: RwSpinlock<Option<Scheduler>> = RwSpinlock::new(None);

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

    pub unsafe fn exec_next_runnable(&mut self) -> ! {
        // ensure no processes are currently running
        assert!(
            self.states
                .entry(ProcessState::Running)
                .or_default()
                .is_empty()
        );

        let Some((_, next_pid)) = self.states.entry(ProcessState::Runnable).or_default().pop()
        else {
            panic!("No runnable processes");
        };

        self.change_state(next_pid, ProcessState::Running);
        self.current_pid = next_pid;

        let Some(next_process) = self.processes.get_mut(&next_pid) else {
            panic!("No process exists with PID {next_pid:?}");
        };

        next_process.reset_last_scheduled();
        unsafe {
            next_process.exec();
        }
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

pub fn add_process(process: Process) {
    let mut scheduler = SCHEDULER.write();
    let scheduler = scheduler
        .as_mut()
        .expect("Scheduler has not been initialized");
    scheduler.add(process);
}

pub unsafe fn exec_next_runnable() -> ! {
    let mut scheduler = SCHEDULER.write();
    let scheduler = scheduler
        .as_mut()
        .expect("Scheduler has not been initialized");
    unsafe {
        scheduler.exec_next_runnable();
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
