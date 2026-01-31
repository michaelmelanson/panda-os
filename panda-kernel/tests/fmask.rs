#![no_std]
#![no_main]

use x86_64::registers::{model_specific::SFMask, rflags::RFlags};

panda_kernel::test_harness!(fmask_clears_dangerous_flags);

fn fmask_clears_dangerous_flags() {
    let fmask = SFMask::read();

    assert!(
        !fmask.is_empty(),
        "FMASK MSR should be non-zero after init"
    );
    assert!(
        fmask.contains(RFlags::INTERRUPT_FLAG),
        "FMASK should clear IF"
    );
    assert!(
        fmask.contains(RFlags::TRAP_FLAG),
        "FMASK should clear TF"
    );
    assert!(
        fmask.contains(RFlags::DIRECTION_FLAG),
        "FMASK should clear DF"
    );
    assert!(
        fmask.contains(RFlags::ALIGNMENT_CHECK),
        "FMASK should clear AC"
    );
    assert!(
        fmask.contains(RFlags::NESTED_TASK),
        "FMASK should clear NT"
    );
}
