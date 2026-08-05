#![no_std]
#![no_main]

//! `libpanda::graphics::Window` end to end against a real compositor
//! process (plans/userspace-compositor.md, Phase 4).
//!
//! This used to draw a four-quadrant pattern via the raw `surface:` scheme
//! and compare a QEMU screenshot. Real pixel output isn't observable in
//! this phase — the in-kernel compositor holds the only display claim until
//! Phase 5 (see docs on `compositor_test_child`) — so this instead proves
//! the client-visible protocol contract: window creation, buffer attach,
//! commit, and `FrameDone` all round-trip correctly through the real
//! `Window` API and a real (spawned) compositor process. Pixel-level
//! blending/positioning correctness is covered by `compositor`'s own
//! `MemoryTarget`-based unit tests in `userspace/compositor/src/manager.rs`.

use libpanda::environment;
use libpanda::graphics::{Colour, PixelBuffer, Rect, Window};
use libpanda::ipc::Channel;
use libpanda::process;

libpanda::main! {
    environment::log("Window test starting");

    let Ok(compositor_handle) = environment::spawn("file:/initrd/compositor_test_child") else {
        environment::log("FAIL: could not spawn compositor_test_child");
        return 1;
    };
    let Some(channel) = Channel::from_handle_borrowed(compositor_handle) else {
        environment::log("FAIL: compositor handle is not a channel");
        return 1;
    };

    let mut window = match Window::builder()
        .size(400, 300)
        .position(50, 50)
        .visible(true)
        .channel(channel)
        .build()
    {
        Ok(w) => w,
        Err(_) => {
            environment::log("FAIL: Could not create window");
            return 1;
        }
    };
    environment::log("PASS: Created window");

    let (window_width, window_height) = window.size();
    let mut buffer = match PixelBuffer::new(window_width, window_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate buffer");
            return 1;
        }
    };
    environment::log("PASS: Allocated buffer");

    let half_width = window_width / 2;
    let half_height = window_height / 2;
    buffer.fill_rect(Rect::new(0, 0, half_width, half_height), Colour::RED);
    buffer.fill_rect(Rect::new(half_width, 0, half_width, half_height), Colour::GREEN);
    buffer.fill_rect(Rect::new(0, half_height, half_width, half_height), Colour::BLUE);
    buffer.fill_rect(
        Rect::new(half_width, half_height, half_width, half_height),
        Colour::YELLOW,
    );
    environment::log("PASS: Filled buffer with test pattern");

    if window.blit(&buffer, 0, 0).is_err() {
        environment::log("FAIL: Could not blit buffer to window");
        return 1;
    }
    environment::log("PASS: Blitted buffer to window");

    // flush() sends Damage + Commit and blocks for FrameDone — reaching
    // this point is the round-trip proof.
    if window.flush().is_err() {
        environment::log("FAIL: Could not flush window");
        return 1;
    }
    environment::log("PASS: Flushed window (FrameDone received)");

    drop(window);
    let exit_code = process::wait(compositor_handle);
    if exit_code != 0 {
        environment::log("FAIL: compositor_test_child exited with non-zero code");
        return 1;
    }

    0
}
