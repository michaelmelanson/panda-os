# Testing Guide

This document describes how to write and run tests for the Panda kernel.

## Running Tests

```bash
# Run all tests (kernel and userspace)
make test

# Run only kernel tests
make kernel-test

# Run only userspace tests
make userspace-test
```

## Kernel Tests

Kernel tests run inside QEMU and test kernel functionality directly. They are located in `panda-kernel/tests/`.

### Writing a Kernel Test

Create a new file in `panda-kernel/tests/`, for example `panda-kernel/tests/my_feature.rs`:

```rust
#![no_std]
#![no_main]

panda_kernel::test_harness!(test_one, test_two);

fn test_one() {
    assert_eq!(1 + 1, 2);
}

fn test_two() {
    // Test can use kernel APIs
    let boxed = alloc::boxed::Box::new(42);
    assert_eq!(*boxed, 42);
}
```

Key points:
- Use `#![no_std]` and `#![no_main]` attributes
- Use the `test_harness!` macro with a list of test function names
- Each test function takes no arguments and returns nothing
- Use `assert!`, `assert_eq!`, etc. for assertions
- Tests have access to kernel internals including the allocator

### Registering a Kernel Test

Add your test name to `KERNEL_TESTS` in `Makefile`:

```makefile
KERNEL_TESTS := basic heap pci memory scheduler process nx_bit raii apic my_feature
```

### Example: Heap Test

```rust
#![no_std]
#![no_main]

extern crate alloc;
use alloc::{boxed::Box, vec::Vec};

panda_kernel::test_harness!(box_allocation, vec_allocation);

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
}
```

## Userspace Tests

Userspace tests are standalone programs that run as processes on top of the kernel. They are located in `userspace/tests/`.

### Writing a Userspace Test

1. Create a new crate in `userspace/tests/`:

```bash
mkdir -p userspace/tests/my_test/src
```

2. Create `userspace/tests/my_test/Cargo.toml`:

```toml
[package]
name = "my_test"
version = "0.1.0"
edition = "2024"

[dependencies]
libpanda = { path = "../../libpanda" }
```

3. Create `userspace/tests/my_test/src/main.rs`:

```rust
#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    environment::log("My test starting");
    
    // Test logic here...
    
    if some_condition_failed {
        environment::log("FAIL: something went wrong");
        return 1;  // Non-zero exit code fails the test
    }
    
    environment::log("My test passed");
    0  // Zero exit code means success
}
```

4. Create `userspace/tests/my_test/expected.txt` with expected log output:

```
# Comments start with #
My test starting
My test passed
```

### Registering a Userspace Test

Add your test name to `USERSPACE_TESTS` in `Makefile`:

```makefile
USERSPACE_TESTS := vfs_test preempt_test spawn_test yield_test my_test
```

### Tests with Multiple Binaries

Some tests require additional helper programs (e.g., spawn_test needs spawn_child). Define extras in the Makefile:

```makefile
my_test_EXTRAS := my_helper
export my_test_EXTRAS
```

### Userspace API

Tests use the libpanda API organized by resource type:

```rust
use libpanda::environment;  // System operations
use libpanda::file;         // File operations  
use libpanda::process;      // Process operations

// Environment operations (via HANDLE_ENVIRONMENT)
environment::log("message");           // Log to console
environment::open("/path", flags);     // Open file, returns handle
environment::spawn("/path");           // Spawn process
environment::time();                   // Get system time

// File operations (on file handles)
file::read(handle, &mut buf);          // Read from file
file::write(handle, &buf);             // Write to file
file::seek(handle, offset, whence);    // Seek in file
file::stat(handle, &mut stat);         // Get file stats
file::close(handle);                   // Close file

// Process operations (via HANDLE_SELF or child handles)
process::yield_now();                  // Yield CPU
process::exit(code);                   // Exit process
process::getpid();                     // Get process ID
process::wait(child_handle);           // Wait for child
```

### Expected Output Matching

The test framework extracts log messages and verifies they appear in order:

- Lines starting with `#` are comments
- Each non-comment line must appear in the log output
- Lines must appear in the specified order
- Additional log messages between expected lines are allowed

Example `expected.txt`:
```
# VFS test checks file operations
VFS test starting
VFS test passed
```

### Exit Codes

- Exit code 0: Test passed
- Exit code non-zero: Test failed (QEMU exits immediately)
- Timeout (60s default): Test failed

## Test Infrastructure

### Scripts

- `scripts/run-tests.sh` - Runs multiple tests in parallel
- `scripts/run-qemu-test.sh` - Runs a single test in QEMU
- `scripts/setup-kernel-test.sh` - Prepares kernel test environment
- `scripts/setup-userspace-test.sh` - Prepares userspace test environment

### Build Directories

Tests are built to:
- Kernel tests: `build/test-<name>/`
- Userspace tests: `build/utest-<name>/`

Test logs are written to:
- Kernel tests: `build/test-<name>.log`
- Userspace tests: `build/utest-<name>.log`
