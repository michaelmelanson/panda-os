//! Async task executor for kernel tasks.
//!
//! Provides cooperative multitasking for long-running kernel operations
//! using Rust's async/await. Tasks share the kernel stack and are polled
//! by the executor when ready.

pub mod join;
pub mod sleep;
pub mod waker;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use spinning_top::Spinlock;

use log::debug;

static EXECUTOR: Spinlock<Executor> = Spinlock::new(Executor::new());

/// Tracks the currently executing task ID during polling.
/// This allows futures to access their own task ID without unsafe code.
static CURRENT_TASK: Spinlock<Option<TaskId>> = Spinlock::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskId(u64);

pub struct Task {
    future: Pin<Box<dyn Future<Output = ()> + Send>>,
}

pub struct Executor {
    tasks: BTreeMap<TaskId, Task>,
    next_id: u64,
}

/// Result of polling a task
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollResult {
    /// Task completed successfully
    Completed,
    /// Task is pending (blocked on something)
    Pending,
    /// Task not found (may have been removed)
    NotFound,
}

impl Executor {
    pub const fn new() -> Self {
        Self {
            tasks: BTreeMap::new(),
            next_id: 0,
        }
    }

    /// Spawn a new async task.
    ///
    /// Returns the task ID. The task will be added to the scheduler by the caller.
    fn spawn(&mut self, future: impl Future<Output = ()> + Send + 'static) -> TaskId {
        let id = TaskId(self.next_id);
        self.next_id += 1;

        let task = Task {
            future: Box::pin(future),
        };

        self.tasks.insert(id, task);
        debug!("Created kernel task {:?}", id);
        id
    }

    /// Poll a single task.
    ///
    /// Returns the result of polling the task.
    fn poll_task(&mut self, task_id: TaskId) -> PollResult {
        let Some(task) = self.tasks.get_mut(&task_id) else {
            return PollResult::NotFound;
        };

        // Set the current task ID so futures can access it
        *CURRENT_TASK.lock() = Some(task_id);

        let waker = waker::create_waker(task_id);
        let mut context = Context::from_waker(&waker);

        let result = match task.future.as_mut().poll(&mut context) {
            Poll::Ready(()) => {
                debug!("Kernel task {:?} completed", task_id);
                self.tasks.remove(&task_id);
                PollResult::Completed
            }
            Poll::Pending => PollResult::Pending,
        };

        // Clear the current task ID
        *CURRENT_TASK.lock() = None;

        result
    }

    /// Check if a task exists.
    fn task_exists(&self, task_id: TaskId) -> bool {
        self.tasks.contains_key(&task_id)
    }
}

/// Spawn a new async kernel task.
///
/// Creates the task and adds it to the scheduler as runnable.
/// The scheduler will poll it when it's time to run.
pub fn spawn(future: impl Future<Output = ()> + Send + 'static) -> TaskId {
    let task_id = EXECUTOR.lock().spawn(future);

    // Add to scheduler
    let mut scheduler = crate::scheduler::SCHEDULER.write();
    if let Some(scheduler) = scheduler.as_mut() {
        scheduler.add_kernel_task(task_id);
    }

    task_id
}

/// Poll a single kernel task.
///
/// Called by the scheduler when it's this task's turn to run.
pub fn poll_single_task(task_id: TaskId) -> PollResult {
    EXECUTOR.lock().poll_task(task_id)
}

/// Wake a task by marking it as runnable in the scheduler.
///
/// Called by wakers when a task is ready to make progress.
pub(crate) fn wake_task(task_id: TaskId) {
    // Check if task still exists
    let exists = EXECUTOR.lock().task_exists(task_id);
    if !exists {
        return;
    }

    // Tell scheduler this task is ready to run
    let mut scheduler = crate::scheduler::SCHEDULER.write();
    if let Some(scheduler) = scheduler.as_mut() {
        scheduler.change_kernel_task_state(task_id, crate::process::ProcessState::Runnable);
    }
}

/// Get the currently executing task ID.
///
/// Returns Some(task_id) if called from within a future being polled,
/// None otherwise.
pub fn current_task_id() -> Option<TaskId> {
    *CURRENT_TASK.lock()
}
