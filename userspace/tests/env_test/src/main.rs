#![no_std]
#![no_main]

use libpanda::{env, environment, process::Child};

libpanda::main! {
    environment::log("env_test: starting");

    // Set an environment variable in the parent
    env::set("FOO", "bar");
    environment::log("env_test: set FOO=bar");

    // Test 1: Basic inheritance
    environment::log("env_test: Test 1 - inheritance");
    let mut child = match Child::builder("file:/initrd/env_child")
        .args(&["env_child", "inherit"])
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            environment::log("FAIL: spawn inherit child failed");
            return 1;
        }
    };

    let status = match child.wait() {
        Ok(s) => s,
        Err(_) => {
            environment::log("FAIL: wait inherit failed");
            return 1;
        }
    };

    if !status.success() {
        environment::log("FAIL: inherit child failed");
        return 1;
    }

    // Test 2: Override environment variable
    environment::log("env_test: Test 2 - override");
    let mut child = match Child::builder("file:/initrd/env_child")
        .args(&["env_child", "override"])
        .env("FOO", "baz")
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            environment::log("FAIL: spawn override child failed");
            return 1;
        }
    };

    let status = match child.wait() {
        Ok(s) => s,
        Err(_) => {
            environment::log("FAIL: wait override failed");
            return 1;
        }
    };

    if !status.success() {
        environment::log("FAIL: override child failed");
        return 1;
    }

    // Test 3: Clear environment
    environment::log("env_test: Test 3 - env_clear");
    let mut child = match Child::builder("file:/initrd/env_child")
        .args(&["env_child", "clear"])
        .env_clear()
        .env("ONLY", "yes")
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            environment::log("FAIL: spawn clear child failed");
            return 1;
        }
    };

    let status = match child.wait() {
        Ok(s) => s,
        Err(_) => {
            environment::log("FAIL: wait clear failed");
            return 1;
        }
    };

    if !status.success() {
        environment::log("FAIL: clear child failed");
        return 1;
    }

    // Test 4: Empty string value
    environment::log("env_test: Test 4 - empty value");
    env::set("EMPTY", "");
    match env::get("EMPTY") {
        Some(v) if v.is_empty() => {}
        Some(_) => {
            environment::log("FAIL: empty value not preserved");
            return 1;
        }
        None => {
            environment::log("FAIL: empty value var not found");
            return 1;
        }
    }

    // Test 5: Remove variable
    environment::log("env_test: Test 5 - remove");
    env::set("TO_REMOVE", "temp");
    let removed = env::remove("TO_REMOVE");
    if removed != Some(libpanda::String::from("temp")) {
        environment::log("FAIL: remove didn't return old value");
        return 1;
    }
    if env::get("TO_REMOVE").is_some() {
        environment::log("FAIL: variable still exists after remove");
        return 1;
    }

    // Test 6: Remove non-existent variable
    environment::log("env_test: Test 6 - remove nonexistent");
    if env::remove("NONEXISTENT").is_some() {
        environment::log("FAIL: remove nonexistent returned Some");
        return 1;
    }

    // Test 7: Overwrite existing variable
    environment::log("env_test: Test 7 - overwrite");
    env::set("OVERWRITE", "first");
    env::set("OVERWRITE", "second");
    match env::get("OVERWRITE") {
        Some(v) if v == "second" => {}
        _ => {
            environment::log("FAIL: overwrite didn't work");
            return 1;
        }
    }

    environment::log("env_test: all tests passed");
    environment::log("PASS");
    0
}
