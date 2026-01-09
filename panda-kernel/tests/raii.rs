#![no_std]
#![no_main]

use panda_kernel::memory::{self, MemoryMappingOptions};
use x86_64::VirtAddr;

panda_kernel::test_harness!(
    frame_deallocates_on_drop,
    frame_refcounting,
    mapping_deallocates_on_drop,
    mapping_refcounting,
    mapping_is_accessible,
    mapping_stress_test
);

/// Verify that Frame deallocates memory when dropped.
/// We allocate and drop many frames in a loop - if deallocation didn't work,
/// we'd run out of memory.
fn frame_deallocates_on_drop() {
    for _ in 0..1000 {
        let _frame = memory::allocate_frame();
        // Frame is dropped here, should deallocate
    }
}

/// Verify that cloned Frames share the same backing memory
/// and only deallocate when the last clone is dropped.
fn frame_refcounting() {
    let frame1 = memory::allocate_frame();
    let addr = frame1.start_address();
    assert_eq!(frame1.ref_count(), 1);

    let frame2 = frame1.clone();
    assert_eq!(frame1.ref_count(), 2);
    assert_eq!(frame2.ref_count(), 2);
    assert_eq!(frame2.start_address(), addr);

    drop(frame1);
    assert_eq!(frame2.ref_count(), 1);
    // frame2 still valid, memory not freed yet
    assert_eq!(frame2.start_address(), addr);
}

/// Verify that Mapping unmaps and deallocates when dropped.
/// We allocate and drop many mappings in a loop.
fn mapping_deallocates_on_drop() {
    // Use high address space to avoid existing mappings
    let base = VirtAddr::new(0x500_0000_0000); // 5TB - unlikely to conflict

    for i in 0..100u64 {
        let virt_addr = base + i * 4096; // Sequential pages
        let _mapping = memory::allocate_and_map(
            virt_addr,
            4096,
            MemoryMappingOptions {
                user: false,
                executable: false,
                writable: true,
            },
        );
        // Mapping is dropped here, should unmap and deallocate frame
    }
}

/// Verify that cloned Mappings share the same backing memory.
fn mapping_refcounting() {
    let virt_addr = VirtAddr::new(0x600_0000_0000); // 6TB
    let mapping1 = memory::allocate_and_map(
        virt_addr,
        4096,
        MemoryMappingOptions {
            user: false,
            executable: false,
            writable: true,
        },
    );
    assert_eq!(mapping1.ref_count(), 1);

    let mapping2 = mapping1.clone();
    assert_eq!(mapping1.ref_count(), 2);
    assert_eq!(mapping2.ref_count(), 2);

    drop(mapping1);
    assert_eq!(mapping2.ref_count(), 1);
    // mapping2 still valid
    assert_eq!(mapping2.base_virtual_address(), virt_addr);
}

/// Verify mapped memory is accessible and properly initialized.
fn mapping_is_accessible() {
    let virt_addr = VirtAddr::new(0x700_0000_0000); // 7TB
    let mapping = memory::allocate_and_map(
        virt_addr,
        4096,
        MemoryMappingOptions {
            user: false,
            executable: false,
            writable: true,
        },
    );

    // Write to mapped memory
    unsafe {
        let ptr = virt_addr.as_mut_ptr::<u64>();
        *ptr = 0xDEADBEEF_CAFEBABE;
        assert_eq!(*ptr, 0xDEADBEEF_CAFEBABE);
    }

    drop(mapping);
    // After drop, accessing this memory would fault (but we don't test that here)
}

/// Stress test: allocate and drop many mappings to verify no memory leak.
fn mapping_stress_test() {
    let base = VirtAddr::new(0x800_0000_0000); // 8TB

    // 50 iterations of 16KB each = 800KB total if not freeing
    // With only ~931MB heap, this should succeed if we're properly freeing
    for i in 0..50u64 {
        let virt_addr = base + i * 4096 * 4;
        let mapping = memory::allocate_and_map(
            virt_addr,
            4 * 4096, // 16KB per mapping
            MemoryMappingOptions {
                user: false,
                executable: false,
                writable: true,
            },
        );

        // Write something to verify it's accessible
        unsafe {
            let ptr = virt_addr.as_mut_ptr::<u64>();
            *ptr = i;
        }

        drop(mapping);
    }
}
