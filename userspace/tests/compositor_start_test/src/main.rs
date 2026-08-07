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
use libpanda::ipc::Channel;

/// Enough ticks to prove the frame loop runs repeatedly without needing the
/// test to take a noticeable amount of time.
const TICKS: u64 = 10;

libpanda::main! {
    environment::log("Compositor start test: starting");

    // Roadmap M4: spawn the userspace display driver directly, the same way
    // `userspace/tests/display_test`/`compositor_test_child` do — this
    // process is its own `init`, so nothing else will.
    let driver_channel = if let Ok(driver_handle) = environment::spawn("file:/initrd/virtio-gpu") {
        let channel = Channel::from_handle_borrowed(driver_handle.into());
        if let Some(channel) = channel.as_ref() {
            let mut ready_msg = [0u8; 64];
            let _ = channel.recv(&mut ready_msg);
        }
        channel
    } else {
        None
    };

    compositor::server::run(Some(TICKS));

    // See `userspace/tests/keyboard_test`/`display_test` for why the driver
    // needs an explicit shutdown request: it runs forever otherwise.
    if let Some(channel) = driver_channel.as_ref() {
        let _ = channel.send(b"shutdown");
    }

    environment::log("PASS");
    0
}
