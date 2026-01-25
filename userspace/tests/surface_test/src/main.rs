#![no_std]
#![no_main]

use libpanda::environment;
use panda_abi::{FillParams, PixelFormat, SurfaceInfoOut, OP_SURFACE_FILL, OP_SURFACE_INFO, OP_SURFACE_FLUSH};

libpanda::main! {
    environment::log("Surface test starting");

    // Open the framebuffer surface
    let Ok(surface) = environment::open("surface:/fb0", 0, 0) else {
        environment::log("FAIL: Could not open framebuffer");
        return 1;
    };
    environment::log("PASS: Opened framebuffer");

    // Get surface info
    let mut info = SurfaceInfoOut {
        width: 0,
        height: 0,
        format: 0,
        stride: 0,
    };

    let result = libpanda::syscall::send(
        surface,
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

    if info.format != PixelFormat::ARGB8888 as u32 {
        environment::log("FAIL: Unexpected pixel format");
        return 1;
    }
    environment::log("PASS: Got surface info");

    // Draw a simple test pattern:
    // - Red rectangle at top-left
    // - Green rectangle at top-right
    // - Blue rectangle at bottom-left
    // - White rectangle at bottom-right
    let half_width = info.width / 2;
    let half_height = info.height / 2;

    // Red (top-left)
    let fill_params = FillParams {
        x: 0,
        y: 0,
        width: half_width,
        height: half_height,
        color: 0xFFFF0000,
    };
    libpanda::syscall::send(
        surface,
        OP_SURFACE_FILL,
        &fill_params as *const FillParams as usize,
        0,
        0,
        0,
    );

    // Green (top-right)
    let fill_params = FillParams {
        x: half_width,
        y: 0,
        width: half_width,
        height: half_height,
        color: 0xFF00FF00,
    };
    libpanda::syscall::send(
        surface,
        OP_SURFACE_FILL,
        &fill_params as *const FillParams as usize,
        0,
        0,
        0,
    );

    // Blue (bottom-left)
    let fill_params = FillParams {
        x: 0,
        y: half_height,
        width: half_width,
        height: half_height,
        color: 0xFF0000FF,
    };
    libpanda::syscall::send(
        surface,
        OP_SURFACE_FILL,
        &fill_params as *const FillParams as usize,
        0,
        0,
        0,
    );

    // White (bottom-right)
    let fill_params = FillParams {
        x: half_width,
        y: half_height,
        width: half_width,
        height: half_height,
        color: 0xFFFFFFFF,
    };
    libpanda::syscall::send(
        surface,
        OP_SURFACE_FILL,
        &fill_params as *const FillParams as usize,
        0,
        0,
        0,
    );

    // Flush to display
    libpanda::syscall::send(surface, OP_SURFACE_FLUSH, 0, 0, 0, 0);

    environment::log("PASS: Drew test pattern");

    // Signal that screenshot can be taken
    environment::screenshot_ready();

    // Loop forever - test runner will capture screenshot and quit QEMU
    // (Can't use hlt in userspace - would cause General Protection Fault)
    loop {
        // Just spin
    }
}
