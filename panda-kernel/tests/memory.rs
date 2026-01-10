#![no_std]
#![no_main]

use panda_kernel::memory;
use x86_64::PhysAddr;

panda_kernel::test_harness!(
    physical_to_virtual_identity_mapping,
    physical_to_virtual_zero,
    physical_to_virtual_high_address,
    allocate_frame_is_page_aligned,
    allocate_multiple_frames_are_distinct
);

fn physical_to_virtual_identity_mapping() {
    let phys = PhysAddr::new(0x1000);
    let virt = memory::physical_address_to_virtual(phys);
    assert_eq!(virt.as_u64(), phys.as_u64());
}

fn physical_to_virtual_zero() {
    let phys = PhysAddr::new(0);
    let virt = memory::physical_address_to_virtual(phys);
    assert_eq!(virt.as_u64(), 0);
}

fn physical_to_virtual_high_address() {
    let phys = PhysAddr::new(0x1_0000_0000); // 4GB
    let virt = memory::physical_address_to_virtual(phys);
    assert_eq!(virt.as_u64(), 0x1_0000_0000);
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
