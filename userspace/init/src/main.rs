#![no_std]
#![no_main]

extern crate alloc;

mod driver_registry;

use driver_registry::DriverRegistry;
use libpanda::environment;

libpanda::main! {
    // Phase 5a (plans/device-driver-model.md): scan the initrd for driver
    // binaries before any filesystem is mounted. `initrd:` is a kernel
    // scheme backed directly by the in-memory ustar archive (see
    // panda-kernel/src/resource/initrd.rs), so this works even though
    // nothing has been mounted yet — resolving the chicken-and-egg problem
    // where the block driver that would mount the root filesystem must
    // itself be read from somewhere first.
    //
    let mut drivers = DriverRegistry::new();
    drivers.scan("initrd:/drivers");

    // Mount ext2 filesystem from the first block device
    if environment::mount("ext2", "/mnt").is_err() {
        environment::log("init: failed to mount ext2");
        return 1;
    }
    environment::log("init: mounted ext2 at /mnt");

    // Phase 5b: scan the now-mounted root filesystem for additional driver
    // binaries. `scan` adds to the registry without clearing what Phase 5a
    // already found.
    drivers.scan("file:/mnt/drivers");

    // Spawn every discovered driver — see `DriverRegistry::binary_paths`'s
    // doc comment for why this is unconditional rather than filtered by a
    // specific enumerated device's identity. Each driver subscribes to and
    // claims only the device(s) its own `.panda_devices.<bus>` table
    // matches, so spawning a driver whose device isn't present is harmless:
    // it just sits waiting on `EVENT_DEVICE_ADDED` forever.
    for path in drivers.binary_paths() {
        if environment::spawn(path).is_err() {
            environment::log("init: failed to spawn driver");
        }
    }

    // Spawn the compositor. It claims the display and serves windows over
    // the protocol; until the in-kernel compositor is deleted (Phase 5 of
    // plans/userspace-compositor.md) the display is already claimed, so it
    // comes up without output. On startup it registers the `compositor:`
    // scheme (OP_SCHEME_REGISTER, landed as roadmap M2) so other processes
    // can reach it without being one of its children.
    let Ok(_compositor_handle) = environment::spawn("file:/mnt/compositor") else {
        environment::log("init: failed to spawn compositor");
        return 1;
    };

    // Spawn the terminal as init's own child, same as the compositor — it
    // gets its channel to the compositor by opening the `compositor:`
    // scheme (environment::connect), not from being spawned by it. See
    // `compositor::server::Compositor::serve_connects`.
    let Ok(_terminal_handle) = environment::spawn("file:/mnt/terminal") else {
        environment::log("init: failed to spawn terminal");
        return 1;
    };

    // Init's job is done - the terminal and compositor found each other via
    // scheme discovery.
    0
}
