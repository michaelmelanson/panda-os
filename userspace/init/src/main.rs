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

    // Spawn every driver discovered in the initrd *before* mounting —
    // critically including `virtio-blk` (M4 Phase 2,
    // plans/device-driver-model.md): the block driver that `mount("ext2")`
    // below needs must itself come from the initrd, since nothing else is
    // readable yet. See `DriverRegistry::binary_paths`'s doc comment for why
    // spawning is unconditional rather than filtered by a specific
    // enumerated device's identity.
    //
    // Spawning only starts the driver process — it still needs to run,
    // claim its device, and call `OP_SCHEME_REGISTER` before `block:`
    // resolves to anything. Waiting here for each driver's readiness
    // handshake (task #43: "Redesign scheme-open readiness to eliminate the
    // driver-registration race class") closes that race outright, so
    // `mount` below doesn't have to gamble on a blind retry loop —
    // see `libpanda::process::wait_ready`'s doc comment.
    for path in drivers.binary_paths() {
        match environment::spawn(path) {
            Ok(handle) => {
                if libpanda::process::wait_ready(handle).is_err() {
                    environment::log("init: initrd driver did not become ready");
                }
            }
            Err(_) => environment::log("init: failed to spawn initrd driver"),
        }
    }

    // Mount ext2 filesystem from the block device the driver(s) spawned
    // above provide.
    if environment::mount("ext2", "/mnt").is_err() {
        environment::log("init: failed to mount ext2");
        return 1;
    }
    environment::log("init: mounted ext2 at /mnt");

    // Phase 5b: scan the now-mounted root filesystem for additional driver
    // binaries (e.g. virtio-keyboard) and spawn any not already spawned
    // above. `scan` adds to the registry without clearing what Phase 5a
    // already found, and `binary_paths()` only lists distinct paths, but it
    // has no memory of what Phase 5a already spawned — track that
    // ourselves so a driver present in both places (shouldn't normally
    // happen) isn't spawned twice.
    let already_spawned: alloc::vec::Vec<alloc::string::String> = drivers
        .binary_paths()
        .into_iter()
        .map(alloc::string::String::from)
        .collect();
    drivers.scan("file:/mnt/drivers");
    for path in drivers.binary_paths() {
        if already_spawned.iter().any(|s| s == path) {
            continue;
        }
        // Wait for each driver's readiness handshake before moving on, same
        // as the initrd drivers above — critically including `virtio-gpu`,
        // whose `display:` scheme the compositor spawned right after this
        // loop depends on (see `libpanda::process::wait_ready`'s doc
        // comment; task #43).
        match environment::spawn(path) {
            Ok(handle) => {
                if libpanda::process::wait_ready(handle).is_err() {
                    environment::log("init: driver did not become ready");
                }
            }
            Err(_) => environment::log("init: failed to spawn driver"),
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
