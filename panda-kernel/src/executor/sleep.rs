//! Async sleep primitive for kernel tasks.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

pub struct SleepFuture {
    wakeup_time: u64,
    registered: bool,
}

impl SleepFuture {
    pub fn new(duration_ms: u64) -> Self {
        let now = crate::time::uptime_ms();
        Self {
            wakeup_time: now + duration_ms,
            registered: false,
        }
    }
}

impl Future for SleepFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context) -> Poll<()> {
        let now = crate::time::uptime_ms();

        if now >= self.wakeup_time {
            return Poll::Ready(());
        }

        if !self.registered {
            // Get current task ID and register deadline with scheduler
            if let Some(task_id) = super::current_task_id() {
                let mut scheduler = crate::scheduler::SCHEDULER.write();
                if let Some(scheduler) = scheduler.as_mut() {
                    scheduler.register_deadline(task_id, self.wakeup_time);
                }
            }
            self.registered = true;
        }

        Poll::Pending
    }
}

/// Sleep for the specified duration in milliseconds.
pub async fn sleep_ms(duration: u64) {
    SleepFuture::new(duration).await
}
