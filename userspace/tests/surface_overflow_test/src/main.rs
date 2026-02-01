#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::graphics::{Colour, PixelBuffer, Rect, Window};
use libpanda::sys;
use panda_abi::BlitParams;

libpanda::main! {
    environment::log("Surface overflow test starting");

    // Create a small window for testing
    let mut window = match Window::builder()
        .size(100, 100)
        .position(10, 10)
        .visible(true)
        .build()
    {
        Ok(w) => w,
        Err(_) => {
            environment::log("FAIL: Could not create window");
            return 1;
        }
    };
    environment::log("PASS: Created window");

    // Allocate a small pixel buffer
    let buffer = match PixelBuffer::new(4, 4) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate buffer");
            return 1;
        }
    };
    environment::log("PASS: Allocated buffer");

    let window_handle = window.surface().handle();
    let buffer_handle_raw = buffer.handle().as_raw();

    // Test 1: Overflow in expected_src_size via large src_y and src_stride
    let params = BlitParams {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
        buffer_handle: buffer_handle_raw,
        src_x: 0,
        src_y: 0xFFFF_FFFF,
        src_stride: 0x8000_0000,
    };
    let result = sys::surface::blit(window_handle, &params);
    if result < 0 {
        environment::log("PASS: Overflow src_y rejected");
    } else {
        environment::log("FAIL: Overflow src_y accepted");
        return 1;
    }

    // Test 2: Overflow in destination bounds (x + width wraps)
    let params = BlitParams {
        x: 0xFFFF_FFFF,
        y: 0,
        width: 2,
        height: 1,
        buffer_handle: buffer_handle_raw,
        src_x: 0,
        src_y: 0,
        src_stride: 0,
    };
    let result = sys::surface::blit(window_handle, &params);
    if result < 0 {
        environment::log("PASS: Overflow dst x rejected");
    } else {
        environment::log("FAIL: Overflow dst x accepted");
        return 1;
    }

    // Test 3: Overflow in destination bounds (y + height wraps)
    let params = BlitParams {
        x: 0,
        y: 0xFFFF_FFFF,
        width: 1,
        height: 2,
        buffer_handle: buffer_handle_raw,
        src_x: 0,
        src_y: 0,
        src_stride: 0,
    };
    let result = sys::surface::blit(window_handle, &params);
    if result < 0 {
        environment::log("PASS: Overflow dst y rejected");
    } else {
        environment::log("FAIL: Overflow dst y accepted");
        return 1;
    }

    // Test 4: Overflow in src_stride * height multiplication
    let params = BlitParams {
        x: 0,
        y: 0,
        width: 0x8000_0000,
        height: 0x8000_0000,
        buffer_handle: buffer_handle_raw,
        src_x: 0,
        src_y: 0,
        src_stride: 0x8000_0000,
    };
    let result = sys::surface::blit(window_handle, &params);
    if result < 0 {
        environment::log("PASS: Large width/height rejected");
    } else {
        environment::log("FAIL: Large width/height accepted");
        return 1;
    }

    // Test 5: Normal blit still works after overflow attempts
    let mut good_buffer = match PixelBuffer::new(100, 100) {
        Ok(b) => b,
        Err(_) => {
            environment::log("FAIL: Could not allocate good buffer");
            return 1;
        }
    };
    good_buffer.fill_rect(
        Rect::new(0, 0, 100, 100),
        Colour::RED,
    );
    if window.blit(&good_buffer, 0, 0).is_err() {
        environment::log("FAIL: Normal blit rejected after overflow tests");
        return 1;
    }
    environment::log("PASS: Normal blit works after overflow tests");

    environment::log("All overflow tests passed");
    0
}
