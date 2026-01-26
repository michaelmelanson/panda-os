#![no_std]
#![no_main]

use libpanda::buffer::Buffer;
use libpanda::environment;
use libpanda::syscall;
use panda_abi::{
    BlitParams, UpdateParamsIn, OP_SURFACE_BLIT, OP_SURFACE_FLUSH, OP_SURFACE_UPDATE_PARAMS,
};

libpanda::main! {
    environment::log("Partial refresh test starting");

    // Create a 400x400 window at (100, 50)
    let Ok(window) = environment::open("surface:/window", 0, 0) else {
        environment::log("FAIL: Could not open window");
        return 1;
    };
    environment::log("PASS: Opened window");

    let window_width = 400u32;
    let window_height = 400u32;

    let update_params = UpdateParamsIn {
        x: 100,
        y: 50,
        width: window_width,
        height: window_height,
        visible: 1,
    };

    syscall::send(
        window.into(),
        OP_SURFACE_UPDATE_PARAMS,
        &update_params as *const UpdateParamsIn as usize,
        0,
        0,
        0,
    );
    environment::log("PASS: Set window parameters");

    // Allocate buffer for full window
    let full_buffer_size = (window_width * window_height * 4) as usize;
    let Some(mut full_buffer) = Buffer::alloc(full_buffer_size) else {
        environment::log("FAIL: Could not allocate full buffer");
        return 1;
    };

    // Fill entire window with blue
    let full_buffer_ptr = full_buffer.as_mut_slice().as_mut_ptr() as *mut u32;
    unsafe {
        for i in 0..(window_width * window_height) {
            *full_buffer_ptr.offset(i as isize) = 0xFF0000FF; // Blue
        }
    }

    let blit_full = BlitParams {
        x: 0,
        y: 0,
        width: window_width,
        height: window_height,
        buffer_handle: full_buffer.handle().into(),
    };

    syscall::send(
        window.into(),
        OP_SURFACE_BLIT,
        &blit_full as *const BlitParams as usize,
        0,
        0,
        0,
    );
    syscall::send(window.into(), OP_SURFACE_FLUSH, 0, 0, 0, 0);
    environment::log("PASS: Filled window with blue");

    // Now create buffers for partial updates
    let partial_width = 200u32;
    let partial_height = 200u32;
    let partial_buffer_size = (partial_width * partial_height * 4) as usize;

    // Update top-left quarter with red (use separate buffer)
    let Some(mut red_buffer) = Buffer::alloc(partial_buffer_size) else {
        environment::log("FAIL: Could not allocate red buffer");
        return 1;
    };

    let red_buffer_ptr = red_buffer.as_mut_slice().as_mut_ptr() as *mut u32;
    unsafe {
        for i in 0..(partial_width * partial_height) {
            *red_buffer_ptr.offset(i as isize) = 0xFFFF0000; // Red
        }
    }

    let blit_red = BlitParams {
        x: 0,
        y: 0,
        width: partial_width,
        height: partial_height,
        buffer_handle: red_buffer.handle().into(),
    };

    syscall::send(
        window.into(),
        OP_SURFACE_BLIT,
        &blit_red as *const BlitParams as usize,
        0,
        0,
        0,
    );
    syscall::send(window.into(), OP_SURFACE_FLUSH, 0, 0, 0, 0);
    environment::log("PASS: Updated top-left quarter with red");

    // Update bottom-right quarter with green (use separate buffer)
    let Some(mut green_buffer) = Buffer::alloc(partial_buffer_size) else {
        environment::log("FAIL: Could not allocate green buffer");
        return 1;
    };

    let green_buffer_ptr = green_buffer.as_mut_slice().as_mut_ptr() as *mut u32;
    unsafe {
        for i in 0..(partial_width * partial_height) {
            *green_buffer_ptr.offset(i as isize) = 0xFF00FF00; // Green
        }
    }

    let blit_green = BlitParams {
        x: 200,
        y: 200,
        width: partial_width,
        height: partial_height,
        buffer_handle: green_buffer.handle().into(),
    };

    syscall::send(
        window.into(),
        OP_SURFACE_BLIT,
        &blit_green as *const BlitParams as usize,
        0,
        0,
        0,
    );
    syscall::send(window.into(), OP_SURFACE_FLUSH, 0, 0, 0, 0);
    environment::log("PASS: Updated bottom-right quarter with green");

    environment::log("PASS: Partial refresh test complete");

    // Give compositor time to process the flush before taking screenshot
    for _ in 0..10 {
        libpanda::process::yield_now();
    }

    // Signal that screenshot can be taken
    environment::screenshot_ready();

    // Loop forever
    loop {}
}
