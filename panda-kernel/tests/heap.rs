#![no_std]
#![no_main]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};

panda_kernel::test_harness!(box_allocation, vec_allocation, large_allocation);

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
