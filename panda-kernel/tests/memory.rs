#![no_std]
#![no_main]

use panda_kernel::memory;
use x86_64::PhysAddr;

panda_kernel::test_harness!(
    mmio_region_is_in_higher_half,
    physical_mapping_is_accessible,
    allocate_frame_is_page_aligned,
    allocate_multiple_frames_are_distinct,
    frame_virtual_address_is_heap,
    kernel_mapped_to_higher_half,
    kernel_relocation_verification
);

/// Test that PhysicalMapping allocations are in the dedicated MMIO region.
fn mmio_region_is_in_higher_half() {
    // Verify the MMIO region constant is correct
    assert_eq!(memory::MMIO_REGION_BASE, 0xffff_9000_0000_0000);

    // Allocate a frame to use as a test "device"
    let frame = memory::allocate_frame();
    let phys_addr = frame.start_address();

    // Create a physical mapping
    let mapping = memory::PhysicalMapping::new(phys_addr, 4096);

    // Verify the virtual address is in the MMIO region
    let virt = mapping.virt_addr();
    assert!(
        virt.as_u64() >= memory::MMIO_REGION_BASE,
        "PhysicalMapping should be in MMIO region: got {:#x}, expected >= {:#x}",
        virt.as_u64(),
        memory::MMIO_REGION_BASE
    );
    assert!(
        virt.as_u64() < memory::MMIO_REGION_BASE + 0x1000_0000_0000, // 16 TB region
        "PhysicalMapping should be within MMIO region bounds"
    );
}

/// Test that PhysicalMapping provides accessible read/write.
fn physical_mapping_is_accessible() {
    // Allocate a frame to use as a test "device"
    let frame = memory::allocate_frame();
    let phys_addr = frame.start_address();

    // Create a physical mapping
    let mapping = memory::PhysicalMapping::new(phys_addr, 4096);

    // Write via physical mapping
    mapping.write::<u32>(0, 0xCAFEBABE);
    mapping.write::<u32>(4, 0xDEADBEEF);

    // Read back via physical mapping
    let val1: u32 = mapping.read(0);
    let val2: u32 = mapping.read(4);

    assert_eq!(val1, 0xCAFEBABE);
    assert_eq!(val2, 0xDEADBEEF);
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

/// Test that Frame::virtual_address() returns a heap address.
fn frame_virtual_address_is_heap() {
    let frame = memory::allocate_frame();
    let virt = frame.virtual_address();

    // Should be in heap region (0xffff_a000...)
    assert!(
        virt.as_u64() >= memory::KERNEL_HEAP_BASE,
        "Frame virtual address should be in heap region: got {:#x}, expected >= {:#x}",
        virt.as_u64(),
        memory::KERNEL_HEAP_BASE
    );

    // Should be below kernel image region
    assert!(
        virt.as_u64() < memory::KERNEL_IMAGE_BASE,
        "Frame virtual address should be below kernel image: got {:#x}, expected < {:#x}",
        virt.as_u64(),
        memory::KERNEL_IMAGE_BASE
    );
}

/// Test that the kernel image base is in higher half.
fn kernel_mapped_to_higher_half() {
    // Verify the kernel image base constant is in the expected higher-half range
    assert_eq!(memory::KERNEL_IMAGE_BASE, 0xffff_c000_0000_0000);

    // Verify it's above the MMIO region
    assert!(memory::KERNEL_IMAGE_BASE > memory::MMIO_REGION_BASE);
}

/// Test that kernel relocations were applied correctly and we're running
/// in the higher half by checking the address of a function.
fn kernel_relocation_verification() {
    // Get the address of this test function - if we're running in higher half,
    // the function pointer should be in the KERNEL_IMAGE_BASE region
    let fn_ptr = kernel_relocation_verification as *const () as u64;

    assert!(
        fn_ptr >= memory::KERNEL_IMAGE_BASE,
        "test function should be in kernel image region: got {:#x}, expected >= {:#x}",
        fn_ptr,
        memory::KERNEL_IMAGE_BASE
    );

    // Also verify that a static variable address is in the higher half
    static TEST_STATIC: u64 = 0xDEADBEEF;
    let static_addr = &TEST_STATIC as *const u64 as u64;

    assert!(
        static_addr >= memory::KERNEL_IMAGE_BASE,
        "static variable should be in kernel image region: got {:#x}, expected >= {:#x}",
        static_addr,
        memory::KERNEL_IMAGE_BASE
    );
}
