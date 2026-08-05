#![no_std]
#![no_main]

//! Boot the userspace compositor and run a bounded number of frames.
//!
//! Until Phase 5 of plans/userspace-compositor.md deletes the in-kernel
//! compositor, its permanent claim makes `display:` `Busy` for everyone
//! else — so this test's job is to prove the compositor process comes up,
//! survives that refusal, and ticks. It runs the service in-process (the
//! same `compositor::server::run` the `compositor` binary calls) with a
//! tick budget, so it terminates instead of looping forever.

use libpanda::environment;

/// Enough ticks to prove the frame loop runs repeatedly without needing the
/// test to take a noticeable amount of time.
const TICKS: u64 = 10;

libpanda::main! {
    environment::log("Compositor start test: starting");

    compositor::server::run(Some(TICKS));

    environment::log("PASS");
    0
}
