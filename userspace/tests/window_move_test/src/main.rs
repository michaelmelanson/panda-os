#![no_std]
#![no_main]

use libpanda::buffer::Buffer;
use libpanda::environment;
use libpanda::syscall;
use panda_abi::{
    BlitParams, UpdateParamsIn, OP_SURFACE_BLIT, OP_SURFACE_FLUSH,
    OP_SURFACE_UPDATE_PARAMS,
};

libpanda::main! {
    environment::log("Window move test starting");

    // Create a window
    let Ok(window) = environment::open("surface:/window", 0, 0) else {
        environment::log("FAIL: Could not open window");
        return 1;
    };
    environment::log("PASS: Opened window");

    let window_width = 200u32;
    let window_height = 150u32;

    // Position window at (50, 50) initially
    let update_params = UpdateParamsIn {
        x: 50,
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
    environment::log("PASS: Set initial window position (50, 50)");

    // Allocate buffer
    let buffer_size = (window_width * window_height * 4) as usize;
    let Some(mut buffer) = Buffer::alloc(buffer_size) else {
        environment::log("FAIL: Could not allocate buffer");
        return 1;
    };

    // Fill with distinctive red color
    let buffer_ptr = buffer.as_mut_slice().as_mut_ptr() as *mut u32;
    unsafe {
        for i in 0..(window_width * window_height) {
            *buffer_ptr.offset(i as isize) = 0xFFFF0000; // Red
        }
    }

    let blit_params = BlitParams {
        x: 0,
        y: 0,
        width: window_width,
        height: window_height,
        buffer_handle: buffer.handle().into(),
    };

    syscall::send(
        window.into(),
        OP_SURFACE_BLIT,
        &blit_params as *const BlitParams as usize,
        0,
        0,
        0,
    );
    syscall::send(window.into(), OP_SURFACE_FLUSH, 0, 0, 0, 0);
    environment::log("PASS: Displayed red window at (50, 50)");

    // Now move the window to a new position (300, 200)
    let move_params = UpdateParamsIn {
        x: 300,
        y: 200,
        width: window_width,
        height: window_height,
        visible: 1,
    };

    syscall::send(
        window.into(),
        OP_SURFACE_UPDATE_PARAMS,
        &move_params as *const UpdateParamsIn as usize,
        0,
        0,
        0,
    );

    // Flush to apply the move
    syscall::send(window.into(), OP_SURFACE_FLUSH, 0, 0, 0, 0);
    environment::log("PASS: Moved window to (300, 200)");

    environment::log("PASS: Window move test complete");

    // Signal that screenshot can be taken
    environment::screenshot_ready();

    // Loop forever
    loop {}
}
