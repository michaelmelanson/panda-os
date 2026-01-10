#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use core::sync::atomic::{AtomicUsize, Ordering};

use panda_kernel::apic;
use panda_kernel::interrupts;
use x86_64::structures::idt::InterruptStackFrame;

/// Timer IRQ line (IRQ 0 = vector 0x20)
const TIMER_IRQ: u8 = 0;

panda_kernel::test_harness!(
    calibration_produces_nonzero_ticks,
    calibration_produces_reasonable_frequency,
    timer_oneshot_fires_interrupt,
    timer_stop_prevents_interrupt,
    timer_fires_multiple_times,
    timer_resumes_execution_after_interrupt
);

/// Timer calibration should produce a non-zero ticks_per_ms value.
fn calibration_produces_nonzero_ticks() {
    let ticks = apic::ticks_per_ms();
    assert!(ticks > 0, "ticks_per_ms should be non-zero after calibration");
}

/// Timer calibration should produce a reasonable frequency.
/// Typical APIC timer frequencies are 10,000 - 1,000,000 ticks/ms depending on bus speed.
fn calibration_produces_reasonable_frequency() {
    let ticks = apic::ticks_per_ms();
    assert!(ticks > 1000, "ticks_per_ms {} is suspiciously low", ticks);
    assert!(ticks < 10_000_000, "ticks_per_ms {} is suspiciously high", ticks);
}

static TIMER_FIRED: AtomicUsize = AtomicUsize::new(0);

extern "x86-interrupt" fn test_timer_handler(_stack_frame: InterruptStackFrame) {
    TIMER_FIRED.fetch_add(1, Ordering::SeqCst);
    apic::eoi();
}

/// Setting a one-shot timer should cause an interrupt to fire.
fn timer_oneshot_fires_interrupt() {
    // Install our test handler
    interrupts::set_irq_handler(TIMER_IRQ, Some(test_timer_handler));

    // Reset counter
    TIMER_FIRED.store(0, Ordering::SeqCst);

    // Set a 1ms timer
    apic::set_timer_oneshot(1);

    // Busy-wait for the interrupt (with timeout)
    let mut spins = 0u32;
    while TIMER_FIRED.load(Ordering::SeqCst) == 0 {
        core::hint::spin_loop();
        spins += 1;
        if spins > 100_000_000 {
            panic!("Timer interrupt did not fire within timeout");
        }
    }

    assert!(
        TIMER_FIRED.load(Ordering::SeqCst) >= 1,
        "Timer interrupt should have fired"
    );

    // Restore default handler
    interrupts::set_irq_handler(TIMER_IRQ, None);
}

/// Timer can fire multiple times when restarted in the handler.
fn timer_fires_multiple_times() {
    static MULTI_TIMER_COUNT: AtomicUsize = AtomicUsize::new(0);

    extern "x86-interrupt" fn multi_timer_handler(_stack_frame: InterruptStackFrame) {
        MULTI_TIMER_COUNT.fetch_add(1, Ordering::SeqCst);
        apic::eoi();
        // Restart timer for next interrupt
        apic::set_timer_oneshot(1);
    }

    // Install handler that restarts timer
    interrupts::set_irq_handler(TIMER_IRQ, Some(multi_timer_handler));
    MULTI_TIMER_COUNT.store(0, Ordering::SeqCst);

    // Start first timer
    apic::set_timer_oneshot(1);

    // Wait for multiple interrupts
    let mut spins = 0u32;
    while MULTI_TIMER_COUNT.load(Ordering::SeqCst) < 5 {
        core::hint::spin_loop();
        spins += 1;
        if spins > 500_000_000 {
            panic!(
                "Only got {} timer interrupts, expected at least 5",
                MULTI_TIMER_COUNT.load(Ordering::SeqCst)
            );
        }
    }

    // Stop the timer
    apic::stop_timer();

    let count = MULTI_TIMER_COUNT.load(Ordering::SeqCst);
    assert!(count >= 5, "Expected at least 5 timer interrupts, got {}", count);

    // Restore default handler
    interrupts::set_irq_handler(TIMER_IRQ, None);
}

/// Execution resumes correctly after timer interrupt.
fn timer_resumes_execution_after_interrupt() {
    static RESUME_COUNT: AtomicUsize = AtomicUsize::new(0);
    static LOOP_PROGRESS: AtomicUsize = AtomicUsize::new(0);

    extern "x86-interrupt" fn resume_timer_handler(_stack_frame: InterruptStackFrame) {
        RESUME_COUNT.fetch_add(1, Ordering::SeqCst);
        apic::eoi();
    }

    // Install handler
    interrupts::set_irq_handler(TIMER_IRQ, Some(resume_timer_handler));
    RESUME_COUNT.store(0, Ordering::SeqCst);
    LOOP_PROGRESS.store(0, Ordering::SeqCst);

    // Set a timer
    apic::set_timer_oneshot(1);

    // Run a loop that should be interrupted but continue after
    for i in 0..10_000_000u64 {
        LOOP_PROGRESS.store(i as usize, Ordering::SeqCst);
        core::hint::black_box(i);
    }

    let progress = LOOP_PROGRESS.load(Ordering::SeqCst);
    let interrupts = RESUME_COUNT.load(Ordering::SeqCst);

    // Loop should have completed
    assert!(
        progress >= 9_999_999,
        "Loop should have completed, only got to {}",
        progress
    );

    // At least one interrupt should have fired
    assert!(
        interrupts >= 1,
        "At least one timer interrupt should have fired"
    );

    // Restore default handler
    interrupts::set_irq_handler(TIMER_IRQ, None);
}

/// Stopping the timer should prevent interrupts from firing.
fn timer_stop_prevents_interrupt() {
    // Install our test handler
    interrupts::set_irq_handler(TIMER_IRQ, Some(test_timer_handler));

    // Reset counter
    TIMER_FIRED.store(0, Ordering::SeqCst);

    // Set a long timer (100ms)
    apic::set_timer_oneshot(100);

    // Immediately stop it
    apic::stop_timer();

    // Wait a bit to make sure no interrupt fires
    for _ in 0..10_000_000 {
        core::hint::spin_loop();
    }

    assert_eq!(
        TIMER_FIRED.load(Ordering::SeqCst),
        0,
        "Timer interrupt should not have fired after stop"
    );

    // Restore default handler
    interrupts::set_irq_handler(TIMER_IRQ, None);
}
