#![no_std]
#![no_main]

use libpanda::buffer::Buffer;
use libpanda::environment;
use libpanda::syscall;
use panda_abi::{
    BlitParams, UpdateParamsIn, OP_SURFACE_BLIT, OP_SURFACE_FLUSH, OP_SURFACE_UPDATE_PARAMS,
};

libpanda::main! {
    environment::log("Alpha blending test starting");

    let window_width = 350u32;
    let window_height = 250u32;
    let buffer_size = (window_width * window_height * 4) as usize;

    // Create three overlapping windows with semi-transparent colors

    // Window 1: Red with 60% alpha at (50, 50)
    let Ok(window1) = environment::open("surface:/window", 0, 0) else {
        environment::log("FAIL: Could not open window 1");
        return 1;
    };
    environment::log("PASS: Opened window 1 (red)");

    let update_params1 = UpdateParamsIn {
        x: 50,
        y: 50,
        width: window_width,
        height: window_height,
        visible: 1,
    };

    syscall::send(
        window1.into(),
        OP_SURFACE_UPDATE_PARAMS,
        &update_params1 as *const UpdateParamsIn as usize,
        0,
        0,
        0,
    );

    let Some(mut buffer1) = Buffer::alloc(buffer_size) else {
        environment::log("FAIL: Could not allocate buffer 1");
        return 1;
    };

    // Fill with semi-transparent red (60% alpha = 153)
    let buffer1_ptr = buffer1.as_mut_slice().as_mut_ptr() as *mut u32;
    unsafe {
        for i in 0..(window_width * window_height) {
            *buffer1_ptr.offset(i as isize) = 0x99FF0000; // ARGB: 60% alpha, red
        }
    }

    let blit_params1 = BlitParams {
        x: 0,
        y: 0,
        width: window_width,
        height: window_height,
        buffer_handle: buffer1.handle().into(),
    };

    syscall::send(
        window1.into(),
        OP_SURFACE_BLIT,
        &blit_params1 as *const BlitParams as usize,
        0,
        0,
        0,
    );
    syscall::send(window1.into(), OP_SURFACE_FLUSH, 0, 0, 0, 0);
    environment::log("PASS: Created red window with 60% alpha");

    // Window 2: Green with 60% alpha at (180, 90)
    let Ok(window2) = environment::open("surface:/window", 0, 0) else {
        environment::log("FAIL: Could not open window 2");
        return 1;
    };
    environment::log("PASS: Opened window 2 (green)");

    let update_params2 = UpdateParamsIn {
        x: 180,
        y: 90,
        width: window_width,
        height: window_height,
        visible: 1,
    };

    syscall::send(
        window2.into(),
        OP_SURFACE_UPDATE_PARAMS,
        &update_params2 as *const UpdateParamsIn as usize,
        0,
        0,
        0,
    );

    let Some(mut buffer2) = Buffer::alloc(buffer_size) else {
        environment::log("FAIL: Could not allocate buffer 2");
        return 1;
    };

    // Fill with semi-transparent green (60% alpha = 153)
    let buffer2_ptr = buffer2.as_mut_slice().as_mut_ptr() as *mut u32;
    unsafe {
        for i in 0..(window_width * window_height) {
            *buffer2_ptr.offset(i as isize) = 0x9900FF00; // ARGB: 60% alpha, green
        }
    }

    let blit_params2 = BlitParams {
        x: 0,
        y: 0,
        width: window_width,
        height: window_height,
        buffer_handle: buffer2.handle().into(),
    };

    syscall::send(
        window2.into(),
        OP_SURFACE_BLIT,
        &blit_params2 as *const BlitParams as usize,
        0,
        0,
        0,
    );
    syscall::send(window2.into(), OP_SURFACE_FLUSH, 0, 0, 0, 0);
    environment::log("PASS: Created green window with 60% alpha");

    // Window 3: Blue with 60% alpha at (115, 170)
    let Ok(window3) = environment::open("surface:/window", 0, 0) else {
        environment::log("FAIL: Could not open window 3");
        return 1;
    };
    environment::log("PASS: Opened window 3 (blue)");

    let update_params3 = UpdateParamsIn {
        x: 115,
        y: 170,
        width: window_width,
        height: window_height,
        visible: 1,
    };

    syscall::send(
        window3.into(),
        OP_SURFACE_UPDATE_PARAMS,
        &update_params3 as *const UpdateParamsIn as usize,
        0,
        0,
        0,
    );

    let Some(mut buffer3) = Buffer::alloc(buffer_size) else {
        environment::log("FAIL: Could not allocate buffer 3");
        return 1;
    };

    // Fill with semi-transparent blue (60% alpha = 153)
    let buffer3_ptr = buffer3.as_mut_slice().as_mut_ptr() as *mut u32;
    unsafe {
        for i in 0..(window_width * window_height) {
            *buffer3_ptr.offset(i as isize) = 0x990000FF; // ARGB: 60% alpha, blue
        }
    }

    let blit_params3 = BlitParams {
        x: 0,
        y: 0,
        width: window_width,
        height: window_height,
        buffer_handle: buffer3.handle().into(),
    };

    syscall::send(
        window3.into(),
        OP_SURFACE_BLIT,
        &blit_params3 as *const BlitParams as usize,
        0,
        0,
        0,
    );
    syscall::send(window3.into(), OP_SURFACE_FLUSH, 0, 0, 0, 0);
    environment::log("PASS: Created blue window with 60% alpha");

    environment::log("PASS: Alpha blending test complete");

    // Signal that screenshot can be taken
    environment::screenshot_ready();

    // Loop forever
    loop {}
}
