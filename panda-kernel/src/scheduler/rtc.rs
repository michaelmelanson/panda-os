//! Real-Time Clock abstraction based on the CPU's Time Stamp Counter (TSC).
//!
//! This provides a monotonic timestamp for scheduling decisions.

use core::arch::x86_64::_rdtsc;

/// Real-Time Clock timestamp based on the CPU's TSC.
///
/// Used for fair scheduling - processes with lower RTC values (less recently
/// scheduled) are prioritized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RTC(u64);

impl RTC {
    /// Returns a zero timestamp, representing "never scheduled".
    pub fn zero() -> RTC {
        RTC(0)
    }

    /// Returns the current timestamp from the CPU's TSC.
    pub fn now() -> RTC {
        let timestamp = unsafe { _rdtsc() };
        RTC(timestamp)
    }
}
