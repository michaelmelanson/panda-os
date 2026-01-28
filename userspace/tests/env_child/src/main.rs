#![no_std]
#![no_main]

extern crate alloc;

use libpanda::{env, environment};

libpanda::main! { |args|
    // First arg tells us which test case to run
    let test_case = args.get(1).map(|s| s.as_str()).unwrap_or("inherit");

    match test_case {
        "inherit" => {
            // Test basic inheritance: check FOO=bar was inherited
            match env::get("FOO") {
                Some(val) if val == "bar" => {
                    environment::log("inherit: FOO=bar OK");
                }
                Some(val) => {
                    environment::log(&alloc::format!("inherit: FAIL FOO={}", val));
                    return 1;
                }
                None => {
                    environment::log("inherit: FAIL FOO not set");
                    return 1;
                }
            }
        }
        "override" => {
            // Test override: parent sets FOO=bar, spawn overrides to FOO=baz
            match env::get("FOO") {
                Some(val) if val == "baz" => {
                    environment::log("override: FOO=baz OK");
                }
                Some(val) => {
                    environment::log(&alloc::format!("override: FAIL FOO={}", val));
                    return 1;
                }
                None => {
                    environment::log("override: FAIL FOO not set");
                    return 1;
                }
            }
        }
        "clear" => {
            // Test env_clear: should NOT have FOO set
            match env::get("FOO") {
                Some(val) => {
                    environment::log(&alloc::format!("clear: FAIL FOO={} (should be unset)", val));
                    return 1;
                }
                None => {
                    environment::log("clear: FOO unset OK");
                }
            }
            // But should have ONLY=yes which was explicitly set
            match env::get("ONLY") {
                Some(val) if val == "yes" => {
                    environment::log("clear: ONLY=yes OK");
                }
                Some(val) => {
                    environment::log(&alloc::format!("clear: FAIL ONLY={}", val));
                    return 1;
                }
                None => {
                    environment::log("clear: FAIL ONLY not set");
                    return 1;
                }
            }
        }
        _ => {
            environment::log(&alloc::format!("Unknown test case: {}", test_case));
            return 1;
        }
    }

    0
}
