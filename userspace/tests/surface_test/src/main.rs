#![no_std]
#![no_main]

extern crate panda_abi;

use libpanda::{environment, syscall::send, buffer::Buffer};
use panda_abi::{
    BlitParams, FillParams, PixelFormat, SurfaceInfoOut, SurfaceRect, OP_SURFACE_BLIT,
    OP_SURFACE_FILL, OP_SURFACE_FLUSH, OP_SURFACE_INFO,
};

libpanda::main! {
    environment::log("surface_test: Starting");

    // Open the framebuffer surface
    let Ok(surface) = environment::open("surface:/fb0", 0) else {
        environment::log("FAIL: Could not open framebuffer surface");
        return 1;
    };

    environment::log("surface_test: Opened framebuffer");

    // Get surface info
    let mut info = SurfaceInfoOut {
        width: 0,
        height: 0,
        format: 0,
        stride: 0,
    };

    let result = send(
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

    environment::log("surface_test: Got surface info");

    // Test 1: Fill with solid colors (divide screen into 4 quadrants)
    let half_width = info.width / 2;
    let half_height = info.height / 2;

    // Top-left: Red
    let fill_params = FillParams {
        x: 0,
        y: 0,
        width: half_width,
        height: half_height,
        color: 0xFFFF0000, // ARGB: Red
    };
    let result = send(
        surface,
        OP_SURFACE_FILL,
        &fill_params as *const FillParams as usize,
        0,
        0,
        0,
    );
    if result < 0 {
        environment::log("FAIL: Could not fill top-left quadrant");
        return 1;
    }

    // Top-right: Green
    let fill_params = FillParams {
        x: half_width,
        y: 0,
        width: half_width,
        height: half_height,
        color: 0xFF00FF00, // ARGB: Green
    };
    let result = send(
        surface,
        OP_SURFACE_FILL,
        &fill_params as *const FillParams as usize,
        0,
        0,
        0,
    );
    if result < 0 {
        environment::log("FAIL: Could not fill top-right quadrant");
        return 1;
    }

    // Bottom-left: Blue
    let fill_params = FillParams {
        x: 0,
        y: half_height,
        width: half_width,
        height: half_height,
        color: 0xFF0000FF, // ARGB: Blue
    };
    let result = send(
        surface,
        OP_SURFACE_FILL,
        &fill_params as *const FillParams as usize,
        0,
        0,
        0,
    );
    if result < 0 {
        environment::log("FAIL: Could not fill bottom-left quadrant");
        return 1;
    }

    // Bottom-right: Yellow
    let fill_params = FillParams {
        x: half_width,
        y: half_height,
        width: half_width,
        height: half_height,
        color: 0xFFFFFF00, // ARGB: Yellow
    };
    let result = send(
        surface,
        OP_SURFACE_FILL,
        &fill_params as *const FillParams as usize,
        0,
        0,
        0,
    );
    if result < 0 {
        environment::log("FAIL: Could not fill bottom-right quadrant");
        return 1;
    }

    environment::log("surface_test: Filled quadrants");

    // Test 2: Blit a small pattern to center of screen
    let blit_width = 100u32;
    let blit_height = 100u32;
    let blit_size = (blit_width * blit_height * 4) as usize;

    let Some(mut pixel_buffer) = Buffer::alloc(blit_size) else {
        environment::log("FAIL: Could not allocate pixel buffer");
        return 1;
    };

    // Create a gradient pattern (diagonal gradient from black to white)
    let pixels = pixel_buffer.as_mut_slice();
    for y in 0..blit_height {
        for x in 0..blit_width {
            let offset = ((y * blit_width + x) * 4) as usize;
            let intensity = ((x + y) * 255 / (blit_width + blit_height)) as u8;
            pixels[offset] = intensity; // B
            pixels[offset + 1] = intensity; // G
            pixels[offset + 2] = intensity; // R
            pixels[offset + 3] = 255; // A
        }
    }

    // Blit to center of screen
    let blit_x = (info.width - blit_width) / 2;
    let blit_y = (info.height - blit_height) / 2;

    let blit_params = BlitParams {
        x: blit_x,
        y: blit_y,
        width: blit_width,
        height: blit_height,
        buffer_handle: pixel_buffer.handle().as_raw(),
    };

    let result = send(
        surface,
        OP_SURFACE_BLIT,
        &blit_params as *const BlitParams as usize,
        0,
        0,
        0,
    );
    if result < 0 {
        environment::log("FAIL: Could not blit pattern");
        return 1;
    }

    environment::log("surface_test: Blitted pattern");

    // Flush the entire surface
    let result = send(surface, OP_SURFACE_FLUSH, 0, 0, 0, 0);
    if result < 0 {
        environment::log("FAIL: Could not flush surface");
        return 1;
    }

    environment::log("surface_test: Flushed surface");

    // Test 3: Flush a specific region
    let flush_rect = SurfaceRect {
        x: 0,
        y: 0,
        width: 100,
        height: 100,
    };

    let result = send(
        surface,
        OP_SURFACE_FLUSH,
        &flush_rect as *const SurfaceRect as usize,
        0,
        0,
        0,
    );
    if result < 0 {
        environment::log("FAIL: Could not flush region");
        return 1;
    }

    environment::log("PASS: All surface operations succeeded");
    0
}
