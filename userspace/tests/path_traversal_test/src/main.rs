//! End-to-end test for VFS path canonicalization.
//!
//! Verifies that `..` and `.` components in paths are resolved
//! correctly before mount-point matching, preventing directory
//! traversal attacks.

#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::file;

libpanda::main! {
    environment::log("path_traversal_test: Starting");

    // Test 1: Normal path works (baseline)
    environment::log("path_traversal_test: Test 1 - Normal open");
    let Ok(handle) = environment::open("file:/initrd/hello.txt", 0, 0) else {
        environment::log("FAIL: Could not open /initrd/hello.txt");
        return 1;
    };
    file::close(handle);
    environment::log("path_traversal_test: Test 1 passed");

    // Test 2: . components are harmless
    environment::log("path_traversal_test: Test 2 - Dot components");
    let Ok(handle) = environment::open("file:/initrd/./hello.txt", 0, 0) else {
        environment::log("FAIL: /initrd/./hello.txt should work");
        return 1;
    };
    file::close(handle);
    environment::log("path_traversal_test: Test 2 passed");

    // Test 3: .. past root should clamp to root
    // /../../../initrd/hello.txt -> /initrd/hello.txt
    environment::log("path_traversal_test: Test 3 - Dotdot past root clamped");
    let Ok(handle) = environment::open("file:/../../../initrd/hello.txt", 0, 0) else {
        environment::log("FAIL: /../../../initrd/hello.txt should work");
        return 1;
    };
    file::close(handle);
    environment::log("path_traversal_test: Test 3 passed");

    // Test 4: Path that resolves to nonexistent mount should fail
    // /initrd/../nonexistent/file -> /nonexistent/file -> no mount -> error
    environment::log("path_traversal_test: Test 4 - Escape to nonexistent mount");
    if environment::open("file:/initrd/../nonexistent/file", 0, 0).is_ok() {
        environment::log("FAIL: /initrd/../nonexistent/file should not succeed");
        return 1;
    }
    environment::log("path_traversal_test: Test 4 passed");

    // Test 5: Repeated slashes
    environment::log("path_traversal_test: Test 5 - Repeated slashes");
    let Ok(handle) = environment::open("file:///initrd//hello.txt", 0, 0) else {
        environment::log("FAIL: ///initrd//hello.txt should work");
        return 1;
    };
    file::close(handle);
    environment::log("path_traversal_test: Test 5 passed");

    environment::log("path_traversal_test: All tests passed!");
    0
}
