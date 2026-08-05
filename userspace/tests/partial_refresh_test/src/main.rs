#![no_std]
#![no_main]

//! Full then partial buffer updates against a real compositor process
//! (plans/userspace-compositor.md, Phase 4). See `window_test` for why
//! this is a protocol round-trip test rather than a screenshot comparison;
//! damage-rect coalescing itself is covered by `compositor`'s
//! `MemoryTarget`-based `manager::tests::only_committed_damage_is_
//! composited_and_flushed` and `dirty_regions_coalesce_when_overlapping_
//! or_adjacent`.

use libpanda::environment;
use libpanda::graphics::{Colour, PixelBuffer, Window};
use libpanda::ipc::Channel;
use libpanda::process;

libpanda::main! {
    environment::log("Partial refresh test starting");

    let window_width = 400u32;
    let window_height = 400u32;

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
        .position(100, 50)
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

    let mut full_buffer = match PixelBuffer::new(window_width, window_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate full buffer");
            return 1;
        }
    };
    full_buffer.clear(Colour::BLUE);

    if window.blit(&full_buffer, 0, 0).is_err() || window.flush().is_err() {
        environment::log("FAIL: Could not fill window with blue");
        return 1;
    }
    environment::log("PASS: Filled window with blue");

    let partial_width = 200u32;
    let partial_height = 200u32;

    let mut red_buffer = match PixelBuffer::new(partial_width, partial_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate red buffer");
            return 1;
        }
    };
    red_buffer.clear(Colour::RED);

    if window.blit(&red_buffer, 0, 0).is_err() || window.flush().is_err() {
        environment::log("FAIL: Could not update top-left quarter");
        return 1;
    }
    environment::log("PASS: Updated top-left quarter with red");

    let mut green_buffer = match PixelBuffer::new(partial_width, partial_height) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate green buffer");
            return 1;
        }
    };
    green_buffer.clear(Colour::GREEN);

    if window.blit(&green_buffer, 200, 200).is_err() || window.flush().is_err() {
        environment::log("FAIL: Could not update bottom-right quarter");
        return 1;
    }
    environment::log("PASS: Updated bottom-right quarter with green");

    environment::log("PASS: Partial refresh test complete");

    drop(window);
    let exit_code = process::wait(compositor_handle);
    if exit_code != 0 {
        environment::log("FAIL: compositor_test_child exited with non-zero code");
        return 1;
    }

    0
}
