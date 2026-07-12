//! Userspace test for the `scheme:` meta-scheme registry (M2.1).
//!
//! `scheme:` is the honest, enumerable replacement for the removed `*:`
//! discovery hack: `readdir("scheme:/")` lists every registered scheme
//! handler by name. This test pins the exact set of schemes registered at
//! boot in `resource/scheme.rs::init` via `expected.txt` -- deliberately, so
//! that adding a new scheme without updating this test's expected.txt fails
//! loudly instead of silently.

#![no_std]
#![no_main]

use libpanda::{DirEntry, ErrorCode, String, Vec, environment, file, format, process};

libpanda::main! {
    environment::log("scheme_registry_test: starting");

    // Test 1: opendir("scheme:/") and readdir should list every registered
    // scheme, including "scheme" itself (it's registered via the same
    // register_scheme() call as everything else, so it lists itself).
    let handle = match environment::opendir("scheme:/") {
        Ok(h) => h,
        Err(_) => {
            environment::log("FAIL: could not opendir scheme:/");
            process::exit(1);
        }
    };

    let mut names: Vec<String> = Vec::new();
    let mut entry = DirEntry {
        name_len: 0,
        is_dir: false,
        name: [0; 255],
    };

    loop {
        let result = file::readdir(handle, &mut entry);
        if result < 0 {
            environment::log("FAIL: readdir returned error");
            process::exit(1);
        }
        if result == 0 {
            break;
        }

        if entry.is_dir {
            environment::log("FAIL: scheme:/ entry marked as directory");
            process::exit(1);
        }

        names.push(String::from(entry.name()));
    }

    file::close(handle);

    if names.is_empty() {
        environment::log("FAIL: no schemes listed");
        process::exit(1);
    }

    // Test 2: the well-known built-in schemes must all be present.
    for expected in ["file", "console", "keyboard", "surface", "block", "scheme"] {
        if !names.iter().any(|n| n.as_str() == expected) {
            environment::log(&format!(
                "FAIL: scheme '{}' missing from scheme:/ listing",
                expected
            ));
            process::exit(1);
        }
    }
    environment::log("scheme_registry_test: found all expected built-in schemes");

    // Test 3: `scheme:/<name>` has nothing to open yet -- that namespace is
    // reserved for M2.2's userspace-provider metadata. Opening it (and the
    // root) must fail NotFound rather than succeeding or crashing.
    match environment::open("scheme:/file", 0, 0) {
        Err(ErrorCode::NotFound) => {
            environment::log("scheme_registry_test: open scheme:/file refused with NotFound");
        }
        Err(_) => {
            environment::log("FAIL: open scheme:/file failed with wrong error");
            process::exit(1);
        }
        Ok(_) => {
            environment::log("FAIL: open scheme:/file unexpectedly succeeded");
            process::exit(1);
        }
    }

    // Log the discovered set (sorted) so expected.txt pins the full list.
    // This is deliberate: adding a scheme without updating expected.txt
    // should make this test fail loudly rather than silently pass.
    names.sort();
    let mut joined = String::new();
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            joined.push_str(", ");
        }
        joined.push_str(name);
    }
    environment::log(&format!("scheme_registry_test: schemes = [{}]", joined));

    environment::log("PASS");
    0
}
