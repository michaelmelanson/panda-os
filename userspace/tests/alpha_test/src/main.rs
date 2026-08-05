#![no_std]
#![no_main]

//! Three overlapping translucent windows against a real compositor process
//! (plans/userspace-compositor.md, Phase 4). See `window_test` for why this
//! is a protocol round-trip test rather than a screenshot comparison; the
//! blend maths itself (output alpha = src_a + dst_a·(1−src_a)) is covered
//! by `compositor_protocol::blend`'s own unit tests and by `compositor`'s
//! `MemoryTarget`-based `manager::tests::a_translucent_window_is_blended_
//! with_the_background`.

use libpanda::environment;
use libpanda::graphics::{Colour, PixelBuffer, Window};
use libpanda::ipc::Channel;
use libpanda::process;

libpanda::main! {
    environment::log("Alpha blending test starting");

    let window_width = 350u32;
    let window_height = 250u32;

    let Ok(compositor_handle) = environment::spawn("file:/initrd/compositor_test_child") else {
        environment::log("FAIL: could not spawn compositor_test_child");
        return 1;
    };
    let Some(channel) = Channel::from_handle_borrowed(compositor_handle) else {
        environment::log("FAIL: compositor handle is not a channel");
        return 1;
    };

    let mut window1 = match Window::builder()
        .size(window_width, window_height)
        .position(50, 50)
        .visible(true)
        .channel(channel)
        .build()
    {
        Ok(w) => w,
        Err(_) => {
            environment::log("FAIL: Could not open window 1");
            return 1;
        }
    };
    environment::log("PASS: Opened window 1 (red)");

    let mut buffer1 = match PixelBuffer::new(window_width, window_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate buffer 1");
            return 1;
        }
    };
    // 60% alpha = 153.
    buffer1.clear(Colour::rgba(255, 0, 0, 153));

    if window1.blit(&buffer1, 0, 0).is_err() || window1.flush().is_err() {
        environment::log("FAIL: Could not render window 1");
        return 1;
    }
    environment::log("PASS: Created red window with 60% alpha");

    let mut window2 = match window1.create_sibling(
        Window::builder().size(window_width, window_height).position(180, 90).visible(true),
    ) {
        Ok(w) => w,
        Err(_) => {
            environment::log("FAIL: Could not open window 2");
            return 1;
        }
    };
    environment::log("PASS: Opened window 2 (green)");

    let mut buffer2 = match PixelBuffer::new(window_width, window_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate buffer 2");
            return 1;
        }
    };
    buffer2.clear(Colour::rgba(0, 255, 0, 153));

    if window2.blit(&buffer2, 0, 0).is_err() || window2.flush().is_err() {
        environment::log("FAIL: Could not render window 2");
        return 1;
    }
    environment::log("PASS: Created green window with 60% alpha");

    let mut window3 = match window1.create_sibling(
        Window::builder().size(window_width, window_height).position(115, 170).visible(true),
    ) {
        Ok(w) => w,
        Err(_) => {
            environment::log("FAIL: Could not open window 3");
            return 1;
        }
    };
    environment::log("PASS: Opened window 3 (blue)");

    let mut buffer3 = match PixelBuffer::new(window_width, window_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate buffer 3");
            return 1;
        }
    };
    buffer3.clear(Colour::rgba(0, 0, 255, 153));

    if window3.blit(&buffer3, 0, 0).is_err() || window3.flush().is_err() {
        environment::log("FAIL: Could not render window 3");
        return 1;
    }
    environment::log("PASS: Created blue window with 60% alpha");

    environment::log("PASS: Alpha blending test complete");

    drop(window3);
    drop(window2);
    drop(window1);
    let exit_code = process::wait(compositor_handle);
    if exit_code != 0 {
        environment::log("FAIL: compositor_test_child exited with non-zero code");
        return 1;
    }

    0
}
