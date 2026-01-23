#![no_std]
#![no_main]

use panda_kernel::memory;
use x86_64::PhysAddr;

panda_kernel::test_harness!(
    physical_to_virtual_uses_phys_window,
    physical_to_virtual_zero,
    physical_to_virtual_high_address,
    allocate_frame_is_page_aligned,
    allocate_multiple_frames_are_distinct
);

fn physical_to_virtual_uses_phys_window() {
    let phys = PhysAddr::new(0x1000);
    let virt = memory::physical_address_to_virtual(phys);
    // With physical window at PHYS_WINDOW_BASE, virt = PHYS_WINDOW_BASE + phys
    let expected = memory::PHYS_WINDOW_BASE + phys.as_u64();
    assert_eq!(virt.as_u64(), expected);
}

fn physical_to_virtual_zero() {
    let phys = PhysAddr::new(0);
    let virt = memory::physical_address_to_virtual(phys);
    // Physical address 0 maps to PHYS_WINDOW_BASE
    assert_eq!(virt.as_u64(), memory::PHYS_WINDOW_BASE);
}

fn physical_to_virtual_high_address() {
    let phys = PhysAddr::new(0x1_0000_0000); // 4GB
    let virt = memory::physical_address_to_virtual(phys);
    let expected = memory::PHYS_WINDOW_BASE + 0x1_0000_0000;
    assert_eq!(virt.as_u64(), expected);
}

fn allocate_frame_is_page_aligned() {
    let frame = memory::allocate_frame();
    assert!(frame.start_address().is_aligned(4096u64));
}

fn allocate_multiple_frames_are_distinct() {
    let frame1 = memory::allocate_frame();
    let frame2 = memory::allocate_frame();
    let frame3 = memory::allocate_frame();

    assert_ne!(frame1.start_address(), frame2.start_address());
    assert_ne!(frame2.start_address(), frame3.start_address());
    assert_ne!(frame1.start_address(), frame3.start_address());
}
