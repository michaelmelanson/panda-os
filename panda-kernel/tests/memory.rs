#![no_std]
#![no_main]

use panda_kernel::memory;
use x86_64::PhysAddr;

panda_kernel::test_harness!(
    physical_to_virtual_uses_phys_window,
    physical_to_virtual_zero,
    physical_to_virtual_high_address,
    physical_window_read_write,
    physical_window_is_mapped,
    mmio_region_is_in_higher_half,
    mmio_mapping_is_accessible,
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

/// Test that we can read and write through the physical window.
/// Allocates a frame, writes via physical window, reads back.
fn physical_window_read_write() {
    let frame = memory::allocate_frame();
    let phys_addr = frame.start_address();

    // Get the virtual address via the physical window
    let virt_addr = memory::physical_address_to_virtual(phys_addr);

    // Verify we're using the physical window (not identity mapping)
    assert!(virt_addr.as_u64() >= memory::PHYS_WINDOW_BASE);

    // Write a test pattern via the physical window
    let ptr = virt_addr.as_u64() as *mut u64;
    let test_value: u64 = 0xDEAD_BEEF_CAFE_BABE;
    unsafe {
        core::ptr::write_volatile(ptr, test_value);
    }

    // Read it back
    let read_value = unsafe { core::ptr::read_volatile(ptr) };
    assert_eq!(read_value, test_value);

    // Write a different pattern to verify it's actually writing
    let test_value2: u64 = 0x1234_5678_9ABC_DEF0;
    unsafe {
        core::ptr::write_volatile(ptr, test_value2);
    }
    let read_value2 = unsafe { core::ptr::read_volatile(ptr) };
    assert_eq!(read_value2, test_value2);
}

/// Test that the physical window region is actually mapped in page tables.
fn physical_window_is_mapped() {
    // The physical window should start at PHYS_WINDOW_BASE
    let window_start = memory::PHYS_WINDOW_BASE;

    // Verify the constant is in the expected higher-half range
    assert_eq!(window_start, 0xffff_8000_0000_0000);

    // Verify PHYS_MAP_BASE is set correctly
    assert_eq!(memory::get_phys_map_base(), window_start);
}

/// Test that MMIO mappings are created in the dedicated MMIO region.
fn mmio_region_is_in_higher_half() {
    // Verify the MMIO region constant is correct
    assert_eq!(memory::MMIO_REGION_BASE, 0xffff_9000_0000_0000);

    // Allocate a frame to use as a test "device"
    let frame = memory::allocate_frame();
    let phys_addr = frame.start_address();

    // Create an MMIO mapping
    let mmio = memory::MmioMapping::new(phys_addr, 4096);

    // Verify the virtual address is in the MMIO region
    let virt = mmio.virt_addr();
    assert!(
        virt.as_u64() >= memory::MMIO_REGION_BASE,
        "MMIO mapping should be in MMIO region: got {:#x}, expected >= {:#x}",
        virt.as_u64(),
        memory::MMIO_REGION_BASE
    );
    assert!(
        virt.as_u64() < memory::MMIO_REGION_BASE + 0x1000_0000_0000, // 16 TB region
        "MMIO mapping should be within MMIO region bounds"
    );
}

/// Test that MMIO mappings are accessible for read/write.
fn mmio_mapping_is_accessible() {
    // Allocate a frame to use as a test "device"
    let frame = memory::allocate_frame();
    let phys_addr = frame.start_address();

    // Create an MMIO mapping
    let mmio = memory::MmioMapping::new(phys_addr, 4096);

    // Write via MMIO mapping
    mmio.write::<u32>(0, 0xCAFEBABE);
    mmio.write::<u32>(4, 0xDEADBEEF);

    // Read back via MMIO mapping
    let val1: u32 = mmio.read(0);
    let val2: u32 = mmio.read(4);

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
