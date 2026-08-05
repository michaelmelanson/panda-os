#![no_std]
#![no_main]

//! A compositor instance spawned by a test rather than by `init`.
//!
//! This is the counterpart to `compositor_start_test`'s in-process
//! `compositor::server::run` call: several Phase 4 tests (see
//! plans/userspace-compositor.md) need a real `libpanda::graphics::Window`
//! talking to a real compositor process over a real channel, which needs
//! two OS processes. Since the parent that spawns a process is the one
//! that gets a channel to it (`docs/IPC.md` "Handle transfer"), the test
//! is this compositor's parent — the reverse of production, where `init`
//! spawns the compositor and the compositor spawns its clients. The test
//! hands its own channel to `Window::builder().channel(...)` instead of
//! relying on `Channel::parent()`.
//!
//! Bounded by a tick count so it exits instead of looping forever: tests
//! wait for it via `process::wait` after they're done.
//!
//! Why a tick budget and not early-exit-on-disconnect: the natural signal
//! would be "the test's channel closed", but every consumer of this binary
//! shares one `SpawnHandle` (`panda-kernel/src/resource/spawn_handle.rs`)
//! for both the IPC channel *and* the `process::wait` handle. There is no
//! syscall to half-close the channel side while keeping the handle alive
//! for `wait`, so a test can't signal "done talking to you" before it
//! blocks in `wait()` without also losing the ability to reap the child's
//! exit code — every test here calls `process::wait(compositor_handle)`
//! while its `Channel`/`Window` (built from the same handle, borrowed) is
//! still open. Adding a half-close primitive would be a new kernel ABI
//! addition, which is out of scope here (see `compositor_protocol_test`'s
//! doc comment for the same "don't touch panda-kernel this phase"
//! constraint). So this stays a fixed real-time budget; the number just
//! needs to match the pacing that actually exists.
//!
//! `compositor::server::run`'s tick loop paces itself with a real
//! `process::sleep(REFRESH_INTERVAL_MS)` (16 ms) between ticks — it isn't
//! a free spin — so this budget is a real-time floor every test that
//! spawns this binary pays, whether or not it needs all the ticks. At the
//! previous value of 2_000 that floor was 32 real seconds per test, which
//! left almost no slack against the suite's default 60 s per-test timeout
//! under full host parallelism (observed as TIMEOUTs in `alpha_test`,
//! `compositor_protocol_test`, and `window_move_test` on an 8-core host at
//! default parallelism, though not under `MAX_PARALLEL=4`). None of the
//! tests need more than a handful of ticks' worth of real work (the
//! slowest, `compositor_protocol_test`, polls up to ~64 attempts at 4 ms —
//! around 250 ms), so 200 ticks (3.2 s) leaves generous headroom without
//! reintroducing a multi-second floor per test.

use libpanda::environment;

/// Generous enough for any single test's handful of windows and commits;
/// bounded so the test process doesn't hang if something never flushes.
/// See the module doc comment for why this is a fixed tick count rather
/// than an early-exit-on-disconnect.
const TICKS: u64 = 200;

libpanda::main! {
    environment::log("compositor_test_child: starting");
    compositor::server::run(Some(TICKS));
    environment::log("compositor_test_child: finished");
    0
}
