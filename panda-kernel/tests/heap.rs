#![no_std]
#![no_main]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};

panda_kernel::test_harness!(box_allocation, vec_allocation, large_allocation, deallocation_reuses_memory);

fn box_allocation() {
    let boxed = Box::new(42);
    assert_eq!(*boxed, 42);
}

fn vec_allocation() {
    let mut vec = Vec::new();
    for i in 0..100 {
        vec.push(i);
    }
    assert_eq!(vec.len(), 100);
    assert_eq!(vec[50], 50);
}

fn large_allocation() {
    // Allocate 1MB to verify heap has sufficient space
    let large_vec: Vec<u8> = Vec::with_capacity(1024 * 1024);
    assert!(large_vec.capacity() >= 1024 * 1024);
}

fn deallocation_reuses_memory() {
    // This test verifies that memory is actually freed and reused.
    // With a bump allocator, this would run out of memory.
    // With a proper allocator, memory is reused after deallocation.
    //
    // We allocate 100MB per iteration, 20 times = 2GB total.
    // Since we only have ~931MB of heap, this MUST free memory to succeed.
    for i in 0..20 {
        let size = 100 * 1024 * 1024; // 100MB
        let mut vec: Vec<u8> = Vec::with_capacity(size);
        // Touch the memory to ensure it's actually allocated
        vec.push(i as u8);
        assert!(vec.capacity() >= size);
        // vec is dropped here, memory should be freed
    }
    // If we get here without OOM, deallocation is working
    // Total allocated if no freeing: 20 * 100MB = 2GB (exceeds 931MB heap)
    // With freeing: only ~100MB at a time
}
