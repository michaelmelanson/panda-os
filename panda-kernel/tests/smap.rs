#![no_std]
#![no_main]

use x86_64::registers::control::{Cr4, Cr4Flags};

panda_kernel::test_harness!(smap_enabled_after_boot, smap_ac_flag_clear);

fn smap_enabled_after_boot() {
    let cr4 = Cr4::read();
    assert!(
        cr4.contains(Cr4Flags::SUPERVISOR_MODE_ACCESS_PREVENTION),
        "CR4.SMAP should be set after kernel init"
    );
    assert!(
        panda_kernel::memory::smap::is_enabled(),
        "smap::is_enabled() should return true"
    );
}

fn smap_ac_flag_clear() {
    // AC flag should be clear during normal kernel execution
    let rflags: u64;
    unsafe {
        core::arch::asm!("pushfq; pop {}", out(reg) rflags, options(nomem));
    }
    assert!(
        rflags & (1 << 18) == 0,
        "AC flag should be clear during kernel execution"
    );

    // with_userspace_access should temporarily set AC then clear it
    panda_kernel::memory::smap::with_userspace_access(|| {
        let rflags_inner: u64;
        unsafe {
            core::arch::asm!("pushfq; pop {}", out(reg) rflags_inner, options(nomem));
        }
        assert!(
            rflags_inner & (1 << 18) != 0,
            "AC flag should be set inside with_userspace_access"
        );
    });

    // AC should be clear again after the closure
    let rflags_after: u64;
    unsafe {
        core::arch::asm!("pushfq; pop {}", out(reg) rflags_after, options(nomem));
    }
    assert!(
        rflags_after & (1 << 18) == 0,
        "AC flag should be clear after with_userspace_access"
    );
}
