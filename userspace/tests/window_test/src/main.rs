#![no_std]
#![no_main]

use libpanda::buffer::Buffer;
use libpanda::environment;
use libpanda::syscall;
use panda_abi::{
    BlitParams, SurfaceInfoOut, UpdateParamsIn, OP_SURFACE_BLIT, OP_SURFACE_FLUSH,
    OP_SURFACE_INFO, OP_SURFACE_UPDATE_PARAMS,
};

libpanda::main! {
    environment::log("Window test starting");

    // Open a window
    let Ok(window) = environment::open("surface:/window", 0) else {
        environment::log("FAIL: Could not open window");
        return 1;
    };
    environment::log("PASS: Opened window");

    // Get window surface info (initial size is 0x0)
    let mut info = SurfaceInfoOut {
        width: 0,
        height: 0,
        format: 0,
        stride: 0,
    };

    let result = syscall::send(
        window.into(),
        OP_SURFACE_INFO,
        &mut info as *mut SurfaceInfoOut as usize,
        0,
        0,
        0,
    );

    if result < 0 {
        environment::log("FAIL: Could not get surface info");
        return 1;
    }
    environment::log("PASS: Got window surface info");

    // Set window size to 400x300 and position at (50, 50)
    let window_width = 400u32;
    let window_height = 300u32;

    let update_params = UpdateParamsIn {
        x: 50,
        y: 50,
        width: window_width,
        height: window_height,
        visible: 1,
    };

    let result = syscall::send(
        window.into(),
        OP_SURFACE_UPDATE_PARAMS,
        &update_params as *const UpdateParamsIn as usize,
        0,
        0,
        0,
    );

    if result < 0 {
        environment::log("FAIL: Could not update window params");
        return 1;
    }
    environment::log("PASS: Updated window parameters");

    // Allocate a buffer for the window content
    let buffer_size = (window_width * window_height * 4) as usize;
    let Some(mut buffer) = Buffer::alloc(buffer_size) else {
        environment::log("FAIL: Could not allocate buffer");
        return 1;
    };
    environment::log("PASS: Allocated buffer");

    // Fill the buffer with a test pattern (4 colored quadrants)
    let buffer_slice = buffer.as_mut_slice();
    let buffer_ptr = buffer_slice.as_mut_ptr() as *mut u32;
    let half_width = window_width / 2;
    let half_height = window_height / 2;

    unsafe {
        for y in 0..window_height {
            for x in 0..window_width {
                let offset = (y * window_width + x) as isize;
                let color = if y < half_height {
                    if x < half_width {
                        0xFFFF0000 // Red (top-left)
                    } else {
                        0xFF00FF00 // Green (top-right)
                    }
                } else {
                    if x < half_width {
                        0xFF0000FF // Blue (bottom-left)
                    } else {
                        0xFFFFFF00 // Yellow (bottom-right)
                    }
                };
                *buffer_ptr.offset(offset) = color;
            }
        }
    }

    environment::log("PASS: Filled buffer with test pattern");

    // Blit the buffer to the window
    let blit_params = BlitParams {
        x: 0,
        y: 0,
        width: window_width,
        height: window_height,
        buffer_handle: buffer.handle().into(),
    };

    let result = syscall::send(
        window.into(),
        OP_SURFACE_BLIT,
        &blit_params as *const BlitParams as usize,
        0,
        0,
        0,
    );

    if result < 0 {
        environment::log("FAIL: Could not blit buffer to window");
        return 1;
    }
    environment::log("PASS: Blitted buffer to window");

    // Flush the window to display
    let result = syscall::send(window.into(), OP_SURFACE_FLUSH, 0, 0, 0, 0);

    if result < 0 {
        environment::log("FAIL: Could not flush window");
        return 1;
    }
    environment::log("PASS: Flushed window");

    // Signal that screenshot can be taken
    environment::screenshot_ready();

    // Loop forever - test runner will capture screenshot and quit QEMU
    loop {
        // Just spin
    }
}
