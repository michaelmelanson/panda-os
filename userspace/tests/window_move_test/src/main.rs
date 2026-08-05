#![no_std]
#![no_main]

//! Moving a window against a real compositor process
//! (plans/userspace-compositor.md, Phase 4). See `window_test` for why this
//! is a protocol round-trip test rather than a screenshot comparison;
//! repainting both the old and new position is covered by `compositor`'s
//! `MemoryTarget`-based `manager::tests::moving_a_window_repaints_both_
//! the_old_and_new_positions`.

use libpanda::environment;
use libpanda::graphics::{Colour, PixelBuffer, Window};
use libpanda::ipc::Channel;
use libpanda::process;

libpanda::main! {
    environment::log("Window move test starting");

    let window_width = 200u32;
    let window_height = 150u32;

    let Ok(compositor_handle) = environment::spawn("file:/initrd/compositor_test_child") else {
        environment::log("FAIL: could not spawn compositor_test_child");
        return 1;
    };
    let Some(channel) = Channel::from_handle_borrowed(compositor_handle) else {
        environment::log("FAIL: compositor handle is not a channel");
        return 1;
    };

    let mut window = match Window::builder()
        .size(window_width, window_height)
        .position(50, 50)
        .visible(true)
        .channel(channel)
        .build()
    {
        Ok(w) => w,
        Err(_) => {
            environment::log("FAIL: Could not open window");
            return 1;
        }
    };
    environment::log("PASS: Opened window");

    if window.position() != (50, 50) {
        environment::log("FAIL: Window did not start at (50, 50)");
        return 1;
    }
    environment::log("PASS: Set initial window position (50, 50)");

    let mut buffer = match PixelBuffer::new(window_width, window_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate buffer");
            return 1;
        }
    };
    buffer.clear(Colour::RED);

    if window.blit(&buffer, 0, 0).is_err() || window.flush().is_err() {
        environment::log("FAIL: Could not display red window");
        return 1;
    }
    environment::log("PASS: Displayed red window at (50, 50)");

    if window.set_position(300, 200).is_err() {
        environment::log("FAIL: Could not move window");
        return 1;
    }
    if window.position() != (300, 200) {
        environment::log("FAIL: Window position did not update");
        return 1;
    }

    if window.flush().is_err() {
        environment::log("FAIL: Could not flush after move");
        return 1;
    }
    environment::log("PASS: Moved window to (300, 200)");

    environment::log("PASS: Window move test complete");

    drop(window);
    let exit_code = process::wait(compositor_handle);
    if exit_code != 0 {
        environment::log("FAIL: compositor_test_child exited with non-zero code");
        return 1;
    }

    0
}
