//! System uptime tracking.
//!
//! Tracks elapsed time since boot using timer interrupts.

use core::sync::atomic::{AtomicU64, Ordering};

/// System uptime in milliseconds
static UPTIME_MS: AtomicU64 = AtomicU64::new(0);

/// Called from timer interrupt to advance system time.
pub fn tick(interval_ms: u64) {
    UPTIME_MS.fetch_add(interval_ms, Ordering::Relaxed);
}

/// Get current system uptime in milliseconds.
pub fn uptime_ms() -> u64 {
    UPTIME_MS.load(Ordering::Relaxed)
}
