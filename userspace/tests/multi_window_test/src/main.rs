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
    environment::log("Multi-window test starting");

    // Create window 1 - Red window at (50, 50), size 300x200
    let Ok(window1) = environment::open("surface:/window", 0) else {
        environment::log("FAIL: Could not open window 1");
        return 1;
    };

    let update_params1 = UpdateParamsIn {
        x: 50,
        y: 50,
        width: 300,
        height: 200,
        visible: 1,
    };

    let result = syscall::send(
        window1.into(),
        OP_SURFACE_UPDATE_PARAMS,
        &update_params1 as *const UpdateParamsIn as usize,
        0,
        0,
        0,
    );

    if result < 0 {
        environment::log("FAIL: Could not update window 1 params");
        return 1;
    }

    // Allocate buffer for window 1 (red)
    let buffer_size1 = (300 * 200 * 4) as usize;
    let Some(mut buffer1) = Buffer::alloc(buffer_size1) else {
        environment::log("FAIL: Could not allocate buffer 1");
        return 1;
    };

    // Fill buffer1 with red color
    let buffer1_slice = buffer1.as_mut_slice();
    let buffer1_ptr = buffer1_slice.as_mut_ptr() as *mut u32;
    unsafe {
        for i in 0..(300 * 200) {
            *buffer1_ptr.offset(i as isize) = 0xFFFF0000; // Red
        }
    }

    // Blit buffer1 to window 1
    let blit_params1 = BlitParams {
        x: 0,
        y: 0,
        width: 300,
        height: 200,
        buffer_handle: buffer1.handle().into(),
    };

    let result = syscall::send(
        window1.into(),
        OP_SURFACE_BLIT,
        &blit_params1 as *const BlitParams as usize,
        0,
        0,
        0,
    );

    if result < 0 {
        environment::log("FAIL: Could not blit buffer 1");
        return 1;
    }

    // Flush window 1
    syscall::send(window1.into(), OP_SURFACE_FLUSH, 0, 0, 0, 0);
    environment::log("PASS: Created and rendered red window at (50, 50)");

    // Create window 2 - Blue window at (150, 100), size 300x200 (overlaps window 1)
    let Ok(window2) = environment::open("surface:/window", 0) else {
        environment::log("FAIL: Could not open window 2");
        return 1;
    };

    let update_params2 = UpdateParamsIn {
        x: 150,
        y: 100,
        width: 300,
        height: 200,
        visible: 1,
    };

    let result = syscall::send(
        window2.into(),
        OP_SURFACE_UPDATE_PARAMS,
        &update_params2 as *const UpdateParamsIn as usize,
        0,
        0,
        0,
    );

    if result < 0 {
        environment::log("FAIL: Could not update window 2 params");
        return 1;
    }

    // Allocate buffer for window 2 (blue)
    let buffer_size2 = (300 * 200 * 4) as usize;
    let Some(mut buffer2) = Buffer::alloc(buffer_size2) else {
        environment::log("FAIL: Could not allocate buffer 2");
        return 1;
    };

    // Fill buffer2 with blue color
    let buffer2_slice = buffer2.as_mut_slice();
    let buffer2_ptr = buffer2_slice.as_mut_ptr() as *mut u32;
    unsafe {
        for i in 0..(300 * 200) {
            *buffer2_ptr.offset(i as isize) = 0xFF0000FF; // Blue
        }
    }

    // Blit buffer2 to window 2
    let blit_params2 = BlitParams {
        x: 0,
        y: 0,
        width: 300,
        height: 200,
        buffer_handle: buffer2.handle().into(),
    };

    let result = syscall::send(
        window2.into(),
        OP_SURFACE_BLIT,
        &blit_params2 as *const BlitParams as usize,
        0,
        0,
        0,
    );

    if result < 0 {
        environment::log("FAIL: Could not blit buffer 2");
        return 1;
    }

    // Flush window 2
    syscall::send(window2.into(), OP_SURFACE_FLUSH, 0, 0, 0, 0);
    environment::log("PASS: Created and rendered blue window at (150, 100)");

    environment::log("PASS: Multi-window test complete - blue should overlap red");

    // Signal screenshot ready
    environment::screenshot_ready();

    // Loop forever
    loop {}
}
