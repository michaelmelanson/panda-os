#![no_std]
#![no_main]

//! Two windows sharing one client connection to a real compositor process
//! (plans/userspace-compositor.md, Phase 4). See `window_test` for why this
//! is a protocol round-trip test rather than a screenshot comparison.

use libpanda::environment;
use libpanda::graphics::{Colour, PixelBuffer, Window};
use libpanda::ipc::Channel;
use libpanda::process;

libpanda::main! {
    environment::log("Multi-window test starting");

    let Ok(compositor_handle) = environment::spawn("file:/initrd/compositor_test_child") else {
        environment::log("FAIL: could not spawn compositor_test_child");
        return 1;
    };
    let Some(channel) = Channel::from_handle_borrowed(compositor_handle) else {
        environment::log("FAIL: compositor handle is not a channel");
        return 1;
    };

    let mut window1 = match Window::builder()
        .size(300, 200)
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

    let mut buffer1 = match PixelBuffer::new(300, 200) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate buffer 1");
            return 1;
        }
    };
    buffer1.clear(Colour::RED);

    if window1.blit(&buffer1, 0, 0).is_err() || window1.flush().is_err() {
        environment::log("FAIL: Could not render window 1");
        return 1;
    }
    environment::log("PASS: Created and rendered red window at (50, 50)");

    // Second window shares window1's connection (create_sibling) — one
    // channel per process, as the compositor expects (message.rs's doc
    // comment: requests on a channel are answered in the order they were
    // sent, one client per channel).
    let mut window2 = match window1.create_sibling(
        Window::builder().size(300, 200).position(150, 100).visible(true),
    ) {
        Ok(w) => w,
        Err(_) => {
            environment::log("FAIL: Could not open window 2");
            return 1;
        }
    };

    let mut buffer2 = match PixelBuffer::new(300, 200) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate buffer 2");
            return 1;
        }
    };
    buffer2.clear(Colour::BLUE);

    if window2.blit(&buffer2, 0, 0).is_err() || window2.flush().is_err() {
        environment::log("FAIL: Could not render window 2");
        return 1;
    }
    environment::log("PASS: Created and rendered blue window at (150, 100)");

    environment::log("PASS: Multi-window test complete");

    drop(window2);
    drop(window1);
    let exit_code = process::wait(compositor_handle);
    if exit_code != 0 {
        environment::log("FAIL: compositor_test_child exited with non-zero code");
        return 1;
    }

    0
}
