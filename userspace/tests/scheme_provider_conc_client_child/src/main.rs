//! Client helper for `scheme_provider_concurrency_test`.
//!
//! Spawned several times concurrently by the parent test, each instance
//! opens the same "conc:/echo" scheme resource, writes a tag unique to this
//! instance (passed as argv[1]), reads it back, and exits 0 only if the
//! round trip succeeded and echoed back exactly what was written.
//!
//! Several of these racing to open the provider at once is what drives
//! concurrent contention on the kernel's per-provider `AsyncLock` (see
//! `scheme_provider_conc_provider_child` for the deliberate reply delay
//! that widens the contention window, and
//! `panda-kernel/src/resource/scheme.rs` for the lock itself).

#![no_std]
#![no_main]

use libpanda::{environment, file};

libpanda::main! { |args|
    let Some(tag) = args.get(1) else {
        environment::log("FAIL: no tag argument");
        return 1;
    };
    let tag = tag.as_str();

    let Ok(handle) = environment::open("conc:/echo", 0, 0) else {
        environment::log("FAIL: could not open conc:/echo");
        return 1;
    };

    let written = file::write(handle, tag.as_bytes());
    if written != tag.len() as isize {
        environment::log("FAIL: write did not report the expected length");
        return 1;
    }

    let mut readback = [0u8; 64];
    let n = file::read(handle, &mut readback);
    if n < 0 || &readback[..n as usize] != tag.as_bytes() {
        environment::log("FAIL: echo did not match what was written");
        return 1;
    }

    file::close(handle);

    environment::log("scheme_provider_conc_client_child: round trip matched");
    0
}
