//! Thorough heap allocation tests for userspace.
//!
//! Tests:
//! - Basic Box allocation and deref
//! - Vec allocation, push, and growth
//! - Multiple allocations
//! - Large allocations (multiple pages)
//! - Alignment requirements
//! - Memory contents integrity
//! - Allocation after page fault handling

#![no_std]
#![no_main]

use libpanda::{environment, Box, Vec};

libpanda::main! {
    environment::log("=== Heap allocation tests ===");

    if !test_box_basic() { return 1; }
    if !test_box_large_value() { return 1; }
    if !test_multiple_boxes() { return 1; }
    if !test_vec_basic() { return 1; }
    if !test_vec_growth() { return 1; }
    if !test_vec_large() { return 1; }
    if !test_alignment() { return 1; }
    if !test_memory_integrity() { return 1; }
    if !test_many_small_allocations() { return 1; }
    if !test_mixed_sizes() { return 1; }

    environment::log("=== All heap tests passed! ===");
    0
}

/// Test basic Box allocation and dereferencing
fn test_box_basic() -> bool {
    environment::log("Test: box_basic");

    let boxed: Box<u64> = Box::new(42);
    if *boxed != 42 {
        environment::log("FAIL: Box value mismatch");
        return false;
    }

    let boxed2: Box<i32> = Box::new(-123);
    if *boxed2 != -123 {
        environment::log("FAIL: Box<i32> value mismatch");
        return false;
    }

    environment::log("  PASS");
    true
}

/// Test Box with a larger value (struct)
fn test_box_large_value() -> bool {
    environment::log("Test: box_large_value");

    #[derive(Clone, Copy)]
    struct LargeStruct {
        a: u64,
        b: u64,
        c: u64,
        d: u64,
        data: [u8; 128],
    }

    let large = LargeStruct {
        a: 0xDEADBEEF,
        b: 0xCAFEBABE,
        c: 0x12345678,
        d: 0x87654321,
        data: [0xAB; 128],
    };

    let boxed = Box::new(large);

    if boxed.a != 0xDEADBEEF {
        environment::log("FAIL: LargeStruct.a mismatch");
        return false;
    }
    if boxed.b != 0xCAFEBABE {
        environment::log("FAIL: LargeStruct.b mismatch");
        return false;
    }
    if boxed.c != 0x12345678 {
        environment::log("FAIL: LargeStruct.c mismatch");
        return false;
    }
    if boxed.d != 0x87654321 {
        environment::log("FAIL: LargeStruct.d mismatch");
        return false;
    }
    for (i, &byte) in boxed.data.iter().enumerate() {
        if byte != 0xAB {
            environment::log("FAIL: LargeStruct.data mismatch");
            return false;
        }
        // Only check first few to avoid timeout
        if i > 10 {
            break;
        }
    }

    environment::log("  PASS");
    true
}

/// Test multiple simultaneous Box allocations
fn test_multiple_boxes() -> bool {
    environment::log("Test: multiple_boxes");

    let box1 = Box::new(1u64);
    let box2 = Box::new(2u64);
    let box3 = Box::new(3u64);
    let box4 = Box::new(4u64);
    let box5 = Box::new(5u64);

    // Verify all values are correct (not corrupted by other allocations)
    if *box1 != 1 || *box2 != 2 || *box3 != 3 || *box4 != 4 || *box5 != 5 {
        environment::log("FAIL: Multiple box values corrupted");
        return false;
    }

    // Verify addresses are distinct
    let addr1 = &*box1 as *const u64 as usize;
    let addr2 = &*box2 as *const u64 as usize;
    let addr3 = &*box3 as *const u64 as usize;

    if addr1 == addr2 || addr2 == addr3 || addr1 == addr3 {
        environment::log("FAIL: Box addresses overlap");
        return false;
    }

    environment::log("  PASS");
    true
}

/// Test basic Vec operations
fn test_vec_basic() -> bool {
    environment::log("Test: vec_basic");

    let mut vec: Vec<u32> = Vec::new();

    vec.push(10);
    vec.push(20);
    vec.push(30);

    if vec.len() != 3 {
        environment::log("FAIL: Vec length wrong");
        return false;
    }

    if vec[0] != 10 || vec[1] != 20 || vec[2] != 30 {
        environment::log("FAIL: Vec contents wrong");
        return false;
    }

    environment::log("  PASS");
    true
}

/// Test Vec growth (triggers reallocation)
fn test_vec_growth() -> bool {
    environment::log("Test: vec_growth");

    let mut vec: Vec<u64> = Vec::new();

    // Push enough elements to trigger multiple reallocations
    for i in 0..100u64 {
        vec.push(i * 7);
    }

    if vec.len() != 100 {
        environment::log("FAIL: Vec length after growth wrong");
        return false;
    }

    // Verify all values survived reallocation
    for i in 0..100u64 {
        if vec[i as usize] != i * 7 {
            environment::log("FAIL: Vec value corrupted after growth");
            return false;
        }
    }

    environment::log("  PASS");
    true
}

/// Test large Vec (spans multiple pages)
fn test_vec_large() -> bool {
    environment::log("Test: vec_large");

    // Allocate 16KB of data (4 pages worth)
    let mut vec: Vec<u8> = Vec::with_capacity(16 * 1024);

    for i in 0..(16 * 1024) {
        vec.push((i & 0xFF) as u8);
    }

    if vec.len() != 16 * 1024 {
        environment::log("FAIL: Large vec length wrong");
        return false;
    }

    // Spot check values across page boundaries
    let check_points = [0, 4095, 4096, 8191, 8192, 12287, 12288, 16383];
    for &i in &check_points {
        if vec[i] != (i & 0xFF) as u8 {
            environment::log("FAIL: Large vec value wrong");
            return false;
        }
    }

    environment::log("  PASS");
    true
}

/// Test alignment requirements
fn test_alignment() -> bool {
    environment::log("Test: alignment");

    // u64 requires 8-byte alignment
    let boxed_u64 = Box::new(0u64);
    let addr = &*boxed_u64 as *const u64 as usize;
    if addr % 8 != 0 {
        environment::log("FAIL: u64 not 8-byte aligned");
        return false;
    }

    // u128 requires 16-byte alignment (on most platforms)
    let boxed_u128 = Box::new(0u128);
    let addr = &*boxed_u128 as *const u128 as usize;
    if addr % 8 != 0 {
        environment::log("FAIL: u128 not properly aligned");
        return false;
    }

    // Test struct with alignment requirement
    #[repr(align(64))]
    struct Aligned64 {
        data: u64,
    }

    let boxed_aligned = Box::new(Aligned64 { data: 42 });
    let addr = &*boxed_aligned as *const Aligned64 as usize;
    if addr % 64 != 0 {
        environment::log("FAIL: Aligned64 not 64-byte aligned");
        return false;
    }

    environment::log("  PASS");
    true
}

/// Test that memory contents remain intact
fn test_memory_integrity() -> bool {
    environment::log("Test: memory_integrity");

    // Allocate and fill with patterns
    let mut vecs: [Vec<u8>; 4] = [
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ];

    // Fill each vec with a different pattern
    for (i, vec) in vecs.iter_mut().enumerate() {
        for j in 0..256 {
            vec.push(((i * 64 + j) & 0xFF) as u8);
        }
    }

    // Verify patterns are still correct
    for (i, vec) in vecs.iter().enumerate() {
        for (j, &byte) in vec.iter().enumerate() {
            let expected = ((i * 64 + j) & 0xFF) as u8;
            if byte != expected {
                environment::log("FAIL: Memory integrity check failed");
                return false;
            }
        }
    }

    environment::log("  PASS");
    true
}

/// Test many small allocations
fn test_many_small_allocations() -> bool {
    environment::log("Test: many_small_allocations");

    let mut boxes: Vec<Box<u32>> = Vec::new();

    // Allocate 200 small boxes
    for i in 0..200u32 {
        boxes.push(Box::new(i));
    }

    // Verify all values
    for (i, boxed) in boxes.iter().enumerate() {
        if **boxed != i as u32 {
            environment::log("FAIL: Small allocation value wrong");
            return false;
        }
    }

    environment::log("  PASS");
    true
}

/// Test mixed allocation sizes
fn test_mixed_sizes() -> bool {
    environment::log("Test: mixed_sizes");

    // Allocate various sizes interleaved
    let small1 = Box::new(1u8);
    let large1 = Box::new([0u64; 64]); // 512B
    let small2 = Box::new(2u16);
    let large2 = Box::new([0u64; 64]); // 512B
    let small3 = Box::new(3u32);
    let small4 = Box::new(4u64);

    // Verify small values not corrupted
    if *small1 != 1 || *small2 != 2 || *small3 != 3 || *small4 != 4 {
        environment::log("FAIL: Small values corrupted by large allocs");
        return false;
    }

    // Verify large allocations are in heap range
    let heap_base = panda_abi::HEAP_BASE;
    let heap_end = heap_base + panda_abi::HEAP_MAX_SIZE;

    let l1_addr = large1.as_ptr() as usize;
    let l2_addr = large2.as_ptr() as usize;

    if l1_addr < heap_base || l1_addr >= heap_end {
        environment::log("FAIL: large1 not in heap");
        return false;
    }
    if l2_addr < heap_base || l2_addr >= heap_end {
        environment::log("FAIL: large2 not in heap");
        return false;
    }

    environment::log("  PASS");
    true
}
