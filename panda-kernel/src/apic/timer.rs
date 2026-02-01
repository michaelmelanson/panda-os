//! APIC timer calibration and control.
//!
//! Uses the PIT (Programmable Interval Timer) as a reference clock to
//! calibrate the APIC timer frequency.

use core::sync::atomic::{AtomicU32, Ordering};

use log::debug;
use x86_64::instructions::port::Port;

use super::{with_local_apic, TimerDivide, TimerMode};

/// Timer interrupt vector (same as legacy PIC timer for compatibility)
pub const TIMER_VECTOR: u8 = 0x20;

/// PIT frequency in Hz (standard PC timer crystal)
const PIT_FREQUENCY: u32 = 1_193_182;

/// Calibration duration in milliseconds
const CALIBRATION_MS: u32 = 10;

/// Calibrated APIC timer ticks per millisecond
static TICKS_PER_MS: AtomicU32 = AtomicU32::new(0);

/// PIT I/O ports
mod pit {
    pub const CHANNEL0_DATA: u16 = 0x40;
    pub const COMMAND: u16 = 0x43;
}

/// PIT command byte for one-shot mode on channel 0
const PIT_ONESHOT_CMD: u8 = 0b00_11_000_0; // Channel 0, lobyte/hibyte, mode 0 (interrupt on terminal count)

/// Wait for approximately `ms` milliseconds using the PIT.
pub fn pit_wait_ms(ms: u32) {
    let count = (PIT_FREQUENCY * ms) / 1000;
    let count = count.min(0xFFFF) as u16;

    unsafe {
        let mut cmd_port: Port<u8> = Port::new(pit::COMMAND);
        let mut data_port: Port<u8> = Port::new(pit::CHANNEL0_DATA);

        // Configure PIT channel 0 for one-shot mode
        cmd_port.write(PIT_ONESHOT_CMD);

        // Write count (low byte then high byte)
        data_port.write((count & 0xFF) as u8);
        data_port.write((count >> 8) as u8);

        // Poll until count reaches 0
        // Read back command: latch count for channel 0
        loop {
            cmd_port.write(0b11_10_00_00); // Read-back command, channel 0, latch count
            let low = data_port.read();
            let high = data_port.read();
            let current = (high as u16) << 8 | (low as u16);
            if current == 0 || current > count {
                break;
            }
        }
    }
}

/// Calibrate the APIC timer using the PIT as a reference.
///
/// This measures how many APIC timer ticks occur during a known time period.
pub fn calibrate_timer() {
    with_local_apic(|apic| {
        // Configure timer with maximum divider for calibration
        apic.configure_timer(TimerMode::OneShot, TIMER_VECTOR, TimerDivide::By16);

        // Mask timer interrupt during calibration (we just read the count)
        apic.mask_timer();

        // Start APIC timer with maximum count
        apic.set_timer_count(0xFFFF_FFFF);

        // Wait for calibration period
        pit_wait_ms(CALIBRATION_MS);

        // Read how many ticks elapsed
        let elapsed = 0xFFFF_FFFF - apic.timer_count();

        // Calculate ticks per millisecond (accounting for divider)
        let ticks_per_ms = elapsed / CALIBRATION_MS;

        TICKS_PER_MS.store(ticks_per_ms, Ordering::SeqCst);

        // Stop the timer
        apic.set_timer_count(0);

        debug!(
            "APIC timer calibrated: {} ticks/ms ({} MHz bus)",
            ticks_per_ms,
            (ticks_per_ms as u64 * 16) / 1000 // Approximate bus frequency
        );
    });
}

/// Get the calibrated ticks per millisecond.
pub fn ticks_per_ms() -> u32 {
    TICKS_PER_MS.load(Ordering::SeqCst)
}

/// Set up a one-shot timer to fire after `ms` milliseconds.
pub fn set_timer_oneshot(ms: u32) {
    with_local_apic(|apic| {
        let ticks = ticks_per_ms() * ms;
        apic.configure_timer(TimerMode::OneShot, TIMER_VECTOR, TimerDivide::By16);
        apic.unmask_timer();
        apic.set_timer_count(ticks);
    });
}

/// Stop the APIC timer.
pub fn stop_timer() {
    with_local_apic(|apic| {
        apic.mask_timer();
        apic.set_timer_count(0);
    });
}
