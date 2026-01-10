//! Waker abstraction for blocking I/O.
//!
//! A `Waker` allows a process to block waiting for an event (like keyboard input)
//! and be woken up when data is available. The scheduler is device-agnostic -
//! it only knows about wakers, not what device they're associated with.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
use spinning_top::Spinlock;

use crate::process::ProcessId;
use crate::scheduler;

/// A waker that can unblock a process waiting for I/O.
///
/// Devices create wakers and return them via `FsError::WouldBlock`.
/// The syscall layer then blocks the process on the waker.
/// When the device has data, it calls `wake()` to unblock the process.
pub struct Waker {
    /// Whether the waker has been signaled (data available)
    signaled: AtomicBool,
    /// Process waiting on this waker (if any)
    waiting: Spinlock<Option<ProcessId>>,
}

impl Waker {
    /// Create a new waker
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            signaled: AtomicBool::new(false),
            waiting: Spinlock::new(None),
        })
    }

    /// Called by device when data is available.
    /// Wakes the waiting process if any.
    pub fn wake(&self) {
        self.signaled.store(true, Ordering::Release);
        if let Some(pid) = self.waiting.lock().take() {
            scheduler::wake_process(pid);
        }
    }

    /// Check if already signaled (non-blocking check)
    pub fn is_signaled(&self) -> bool {
        self.signaled.load(Ordering::Acquire)
    }

    /// Clear signal after consuming data
    pub fn clear(&self) {
        self.signaled.store(false, Ordering::Release);
    }

    /// Register a process as waiting on this waker.
    /// Called by scheduler when blocking.
    pub fn set_waiting(&self, pid: ProcessId) {
        *self.waiting.lock() = Some(pid);
    }
}

impl Default for Waker {
    fn default() -> Self {
        Self {
            signaled: AtomicBool::new(false),
            waiting: Spinlock::new(None),
        }
    }
}
