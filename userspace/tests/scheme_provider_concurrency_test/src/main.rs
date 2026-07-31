//! Regression test for the `AsyncLock` lost-wakeup bug in
//! `panda-kernel/src/resource/scheme.rs`.
//!
//! `scheme_provider_test` only ever has a single client talking to a
//! userspace scheme provider, so it never exercises concurrent contention
//! on the provider's `AsyncLock` (the primitive that serialises
//! request/response round trips to a single provider). This test spawns
//! three client processes that all try to open the *same* registered
//! scheme at (as close to) the same time, with the provider deliberately
//! delaying its `Open` reply (see `scheme_provider_conc_provider_child`) to
//! force at least two clients to be blocked in `AsyncLock::acquire()`
//! simultaneously.
//!
//! Before the fix, `AsyncLock` tracked waiters in a single-slot `IoWaker`:
//! if two clients both found the lock busy and both called
//! `set_waiting()`, the second silently clobbered the first's entry, so
//! when the lock was released only one of them was ever woken — the other
//! hung forever. That hang shows up here as the harness's TIMEOUT (the
//! `process::wait()` on that stuck child never returns), which is exactly
//! the signal this test is designed to produce against the unfixed code.
//! Every client completing (and reporting a matching echo) is the positive
//! signal that the fix works.

#![no_std]
#![no_main]

use libpanda::ipc::Channel;
use libpanda::process::Child;
use libpanda::{environment, process};

libpanda::main! {
    environment::log("scheme_provider_concurrency_test: starting");

    let Ok(provider_handle) = environment::spawn("file:/initrd/scheme_provider_conc_provider_child")
    else {
        environment::log("FAIL: spawn of provider returned error");
        return 1;
    };

    let Some(to_provider) = Channel::from_handle_borrowed(provider_handle.into()) else {
        environment::log("FAIL: provider handle is not a channel");
        return 1;
    };

    // Wait for the provider to register its scheme before spawning clients.
    let mut msg = [0u8; 64];
    match to_provider.recv(&mut msg) {
        Ok(len) if &msg[..len] == b"ready" => {
            environment::log("scheme_provider_concurrency_test: provider registered conc scheme");
        }
        _ => {
            environment::log("FAIL: did not receive ready signal from provider");
            return 1;
        }
    }

    // Spawn several clients back-to-back, *before* waiting on any of them,
    // so their first "conc:/echo" opens race each other for the provider's
    // AsyncLock. The provider's artificial delay on Open (see
    // scheme_provider_conc_provider_child) widens the window so this is
    // reliable rather than a matter of scheduling luck.
    const TAGS: [&str; 3] = ["clientA", "clientB", "clientC"];
    let mut children: libpanda::Vec<Child> = libpanda::Vec::new();
    for tag in TAGS {
        match Child::spawn_with_args(
            "file:/initrd/scheme_provider_conc_client_child",
            &["scheme_provider_conc_client_child", tag],
        ) {
            Ok(child) => children.push(child),
            Err(_) => {
                environment::log("FAIL: spawn of client returned error");
                return 1;
            }
        }
    }
    environment::log("scheme_provider_concurrency_test: spawned 3 concurrent clients");

    // If Bug 1 (the AsyncLock lost wakeup) is present, at least one of
    // these `wait()` calls hangs forever and the test harness reports
    // TIMEOUT instead of this function ever reaching the log lines below.
    let mut all_ok = true;
    for (i, mut child) in children.into_iter().enumerate() {
        match child.wait() {
            Ok(status) if status.success() => {
                environment::log("scheme_provider_concurrency_test: client completed");
            }
            _ => {
                environment::log("FAIL: a concurrent client did not exit successfully");
                all_ok = false;
                let _ = i;
            }
        }
    }
    if !all_ok {
        return 1;
    }
    environment::log("scheme_provider_concurrency_test: all concurrent clients completed successfully");

    if to_provider.send(b"die").is_err() {
        environment::log("FAIL: could not signal provider to die");
        return 1;
    }
    let exit_code = process::wait(provider_handle);
    if exit_code != 0 {
        environment::log("FAIL: provider exited with a non-zero code");
        return 1;
    }
    environment::log("scheme_provider_concurrency_test: provider exited");

    environment::log("PASS");
    0
}
