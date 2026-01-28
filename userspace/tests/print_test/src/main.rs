//! Test for print! and println! macros.
//!
//! Note: print!/println! output goes to the parent channel, not the kernel log.
//! We use environment::log() for test markers that the test framework checks.

#![no_std]
#![no_main]

use libpanda::{environment, print, println};

libpanda::main! {
    environment::log("=== Print macro tests ===");

    // Test basic println
    println!("Test: basic println");
    println!("  PASS");

    // Test println with no arguments
    println!("Test: empty println");
    println!();
    println!("  PASS");

    // Test print without newline
    println!("Test: print without newline");
    print!("Hello, ");
    print!("world!");
    println!();
    println!("  PASS");

    // Test formatting with integers
    println!("Test: integer formatting");
    let x: i32 = 42;
    let y: i32 = -17;
    println!("  x = {}, y = {}", x, y);
    println!("  PASS");

    // Test formatting with different bases
    println!("Test: hex and binary formatting");
    let n: u32 = 255;
    println!("  dec={} hex={:#x} bin={:#b}", n, n, n);
    println!("  PASS");

    // Test formatting with padding
    println!("Test: padding and alignment");
    println!("  [{:>8}]", 42);
    println!("  [{:<8}]", 42);
    println!("  [{:^8}]", 42);
    println!("  PASS");

    // Test formatting with strings
    println!("Test: string formatting");
    let s = "hello";
    println!("  message: {}", s);
    println!("  PASS");

    // Test formatting with multiple arguments
    println!("Test: multiple arguments");
    println!("  a={} b={} c={} d={}", 1, 2, 3, 4);
    println!("  PASS");

    // Test debug formatting
    println!("Test: debug formatting");
    let arr = [1, 2, 3];
    println!("  array: {:?}", arr);
    println!("  PASS");

    environment::log("=== All print tests passed! ===");
    0
}
