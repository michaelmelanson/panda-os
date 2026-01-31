#![no_std]
#![no_main]

use x86_64::registers::{model_specific::SFMask, rflags::RFlags};

panda_kernel::test_harness!(fmask_is_programmed, fmask_clears_if, fmask_clears_tf, fmask_clears_df, fmask_clears_ac, fmask_clears_nt);

fn fmask_is_programmed() {
    let fmask = SFMask::read();
    assert!(
        !fmask.is_empty(),
        "FMASK MSR should be non-zero after syscall init"
    );
}

fn fmask_clears_if() {
    let fmask = SFMask::read();
    assert!(
        fmask.contains(RFlags::INTERRUPT_FLAG),
        "FMASK should clear IF to disable interrupts on syscall entry"
    );
}

fn fmask_clears_tf() {
    let fmask = SFMask::read();
    assert!(
        fmask.contains(RFlags::TRAP_FLAG),
        "FMASK should clear TF to prevent single-step leaking kernel flow"
    );
}

fn fmask_clears_df() {
    let fmask = SFMask::read();
    assert!(
        fmask.contains(RFlags::DIRECTION_FLAG),
        "FMASK should clear DF to ensure forward string operations"
    );
}

fn fmask_clears_ac() {
    let fmask = SFMask::read();
    assert!(
        fmask.contains(RFlags::ALIGNMENT_CHECK),
        "FMASK should clear AC to prevent alignment check exceptions"
    );
}

fn fmask_clears_nt() {
    let fmask = SFMask::read();
    assert!(
        fmask.contains(RFlags::NESTED_TASK),
        "FMASK should clear NT to prevent IRET interference"
    );
}
