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
            // Register a wakeup deadline with the scheduler so this future
            // gets re-polled once `wakeup_time` arrives, rather than
            // relying on some other event to happen to re-poll it.
            //
            // Which `SchedulableEntity` to register against depends on who
            // is polling this future:
            //
            // - A kernel task spawned via `executor::spawn` (e.g.
            //   `ProviderState::spawn_reader`) is polled by
            //   `Executor::poll_task`, which sets `CURRENT_TASK` for the
            //   duration of the poll — `current_task_id()` returns
            //   `Some(_)` here, and the deadline is registered as a
            //   `KernelTask`.
            // - A process's pending async syscall (e.g. a blocked
            //   `OP_FILE_READ` round-tripping to a scheme provider) is
            //   polled by
            //   `scheduler::poll_pending_syscall` instead, which never
            //   touches `CURRENT_TASK` — it has its own, separate notion of
            //   "current" (`Scheduler::current_process`, set right before
            //   polling). `current_task_id()` returns `None` in this case,
            //   so this falls back to registering against the current
            //   *process* instead.
            //
            // Getting this wrong silently drops the deadline: nothing else
            // ever re-polls a process's blocked pending-syscall future
            // (unlike a kernel task, which the executor's main loop polls
            // unconditionally every tick), so a `sleep_ms` awaited from
            // inside a syscall handler would hang forever instead of ever
            // waking up. Found via `make run`'s full boot
            // (gpu+mouse+keyboard+blk) hanging after "virtio-blk:
            // registered block: scheme", when the kernel still mounted ext2
            // itself and its `OP_ENVIRONMENT_MOUNT` handler slept in a
            // retry loop racing the block driver's startup. That caller is
            // gone (mounting now happens in `userspace/services/fs`), but
            // the dispatch rule below is what makes *any* in-syscall
            // `sleep_ms` work at all.
            let entity = match super::current_task_id() {
                Some(task_id) => Some(crate::scheduler::SchedulableEntity::KernelTask(task_id)),
                None => {
                    crate::scheduler::current_process_id_checked()
                        .map(crate::scheduler::SchedulableEntity::Process)
                }
            };
            if let Some(entity) = entity {
                let mut scheduler = crate::scheduler::SCHEDULER.write();
                if let Some(scheduler) = scheduler.as_mut() {
                    scheduler.register_deadline(entity, self.wakeup_time);
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
