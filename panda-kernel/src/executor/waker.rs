//! Waker implementation for async tasks.

use alloc::sync::Arc;
use alloc::task::Wake;
use core::task::Waker;

use super::TaskId;

pub struct TaskWaker {
    pub task_id: TaskId,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        super::wake_task(self.task_id);
    }
}

/// Create a waker for the given task ID.
pub fn create_waker(task_id: TaskId) -> Waker {
    Waker::from(Arc::new(TaskWaker { task_id }))
}
