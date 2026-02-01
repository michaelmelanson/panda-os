//! System uptime tracking.
//!
//! Tracks elapsed time since boot using timer interrupts and the TSC
//! for sub-millisecond precision.

use core::arch::x86_64::_rdtsc;
use core::sync::atomic::{AtomicU64, Ordering};

/// System uptime in milliseconds
static UPTIME_MS: AtomicU64 = AtomicU64::new(0);

/// TSC value recorded at the last timer tick (used to interpolate within a ms).
static TICK_TSC: AtomicU64 = AtomicU64::new(0);

/// Calibrated TSC ticks per millisecond.
static TSC_TICKS_PER_MS: AtomicU64 = AtomicU64::new(0);

/// Called from timer interrupt to advance system time.
pub fn tick(interval_ms: u64) {
    UPTIME_MS.fetch_add(interval_ms, Ordering::Relaxed);
    TICK_TSC.store(unsafe { _rdtsc() }, Ordering::Relaxed);
}

/// Calibrate the TSC frequency using the PIT as a reference clock.
///
/// Should be called once during early boot. Uses a 10 ms PIT delay to
/// measure how many TSC ticks elapse.
pub fn calibrate_tsc() {
    use x86_64::instructions::port::Port;

    const PIT_FREQUENCY: u32 = 1_193_182;
    const CALIBRATION_MS: u32 = 10;
    const PIT_CHANNEL0_DATA: u16 = 0x40;
    const PIT_COMMAND: u16 = 0x43;
    const PIT_ONESHOT_CMD: u8 = 0b00_11_000_0;

    let count = (PIT_FREQUENCY * CALIBRATION_MS) / 1000;
    let count = count.min(0xFFFF) as u16;

    let tsc_start = unsafe { _rdtsc() };

    unsafe {
        let mut cmd_port: Port<u8> = Port::new(PIT_COMMAND);
        let mut data_port: Port<u8> = Port::new(PIT_CHANNEL0_DATA);

        cmd_port.write(PIT_ONESHOT_CMD);
        data_port.write((count & 0xFF) as u8);
        data_port.write((count >> 8) as u8);

        loop {
            cmd_port.write(0b11_10_00_00);
            let low = data_port.read();
            let high = data_port.read();
            let current = (high as u16) << 8 | (low as u16);
            if current == 0 || current > count {
                break;
            }
        }
    }

    let tsc_end = unsafe { _rdtsc() };
    let tsc_elapsed = tsc_end - tsc_start;
    let ticks_per_ms = tsc_elapsed / CALIBRATION_MS as u64;

    TSC_TICKS_PER_MS.store(ticks_per_ms, Ordering::SeqCst);
    TICK_TSC.store(tsc_end, Ordering::Relaxed);

    log::debug!(
        "TSC calibrated: {} ticks/ms (~{} MHz)",
        ticks_per_ms,
        ticks_per_ms / 1000
    );
}

/// Get current system uptime in milliseconds.
pub fn uptime_ms() -> u64 {
    UPTIME_MS.load(Ordering::Relaxed)
}

/// Get current system uptime in nanoseconds with TSC interpolation.
///
/// Uses the millisecond counter as a base and the TSC to interpolate
/// within the current tick interval for sub-millisecond precision.
pub fn uptime_ns() -> u64 {
    let ticks_per_ms = TSC_TICKS_PER_MS.load(Ordering::Relaxed);
    if ticks_per_ms == 0 {
        // TSC not yet calibrated; fall back to millisecond resolution
        return uptime_ms() * 1_000_000;
    }

    let base_ms = UPTIME_MS.load(Ordering::Relaxed);
    let base_tsc = TICK_TSC.load(Ordering::Relaxed);
    let now_tsc = unsafe { _rdtsc() };

    let delta_tsc = now_tsc.saturating_sub(base_tsc);
    let delta_ns = (delta_tsc * 1_000_000) / ticks_per_ms;

    base_ms * 1_000_000 + delta_ns
}
