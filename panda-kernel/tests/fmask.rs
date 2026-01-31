#![no_std]
#![no_main]

use x86_64::registers::{model_specific::SFMask, rflags::RFlags};

panda_kernel::test_harness!(
    fmask_is_nonzero,
    fmask_clears_interrupt_flag,
    fmask_clears_trap_flag,
    fmask_clears_direction_flag,
    fmask_clears_alignment_check,
    fmask_clears_nested_task
);

fn fmask_is_nonzero() {
    let fmask = SFMask::read();
    assert!(
        !fmask.is_empty(),
        "FMASK MSR should be non-zero after init"
    );
}

fn fmask_clears_interrupt_flag() {
    let fmask = SFMask::read();
    assert!(
        fmask.contains(RFlags::INTERRUPT_FLAG),
        "FMASK should clear IF"
    );
}

fn fmask_clears_trap_flag() {
    let fmask = SFMask::read();
    assert!(
        fmask.contains(RFlags::TRAP_FLAG),
        "FMASK should clear TF"
    );
}

fn fmask_clears_direction_flag() {
    let fmask = SFMask::read();
    assert!(
        fmask.contains(RFlags::DIRECTION_FLAG),
        "FMASK should clear DF"
    );
}

fn fmask_clears_alignment_check() {
    let fmask = SFMask::read();
    assert!(
        fmask.contains(RFlags::ALIGNMENT_CHECK),
        "FMASK should clear AC"
    );
}

fn fmask_clears_nested_task() {
    let fmask = SFMask::read();
    assert!(
        fmask.contains(RFlags::NESTED_TASK),
        "FMASK should clear NT"
    );
}
