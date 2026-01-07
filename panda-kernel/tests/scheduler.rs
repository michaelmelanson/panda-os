#![no_std]
#![no_main]

use panda_kernel::scheduler::RTC;

panda_kernel::test_harness!(
    rtc_zero_is_minimal,
    rtc_now_is_nonzero,
    rtc_now_increases,
    rtc_ordering
);

fn rtc_zero_is_minimal() {
    let zero = RTC::zero();
    let now = RTC::now();
    assert!(zero < now, "RTC::zero() should be less than RTC::now()");
}

fn rtc_now_is_nonzero() {
    let now = RTC::now();
    let zero = RTC::zero();
    assert!(now > zero, "RTC::now() should be greater than zero");
}

fn rtc_now_increases() {
    let first = RTC::now();
    // Do some work to ensure time passes
    for _ in 0..1000 {
        core::hint::black_box(0);
    }
    let second = RTC::now();
    assert!(second > first, "RTC should increase over time");
}

fn rtc_ordering() {
    let zero = RTC::zero();
    let t1 = RTC::now();
    let t2 = RTC::now();

    // Test Ord implementation
    assert!(zero <= zero);
    assert!(zero < t1);
    assert!(t1 <= t2);

    // Test Eq implementation
    assert_eq!(zero, zero);
    assert_eq!(RTC::zero(), RTC::zero());
}
