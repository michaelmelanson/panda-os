#![no_std]
#![no_main]

use libpanda::environment;

libpanda::main! {
    // Mount ext2 filesystem from the first block device
    if environment::mount("ext2", "/mnt").is_err() {
        environment::log("init: failed to mount ext2");
        return 1;
    }
    environment::log("init: mounted ext2 at /mnt");

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
