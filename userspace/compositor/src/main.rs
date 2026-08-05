#![no_std]
#![no_main]

libpanda::main! {
    // `init` spawns the compositor and its graphical clients (e.g. the
    // terminal) independently; clients reach the compositor by opening the
    // `compositor:` scheme this process registers on startup, not by being
    // spawned by it. See `server::run` and `server::Compositor::serve_connects`.
    compositor::server::run(None);
    0
}
