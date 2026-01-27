#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::graphics::{Colour, Rect, Surface};

libpanda::main! {
    environment::log("Surface test starting");

    // Open the framebuffer surface
    let mut surface = match Surface::open("surface:/fb0") {
        Ok(s) => s,
        Err(_) => {
            environment::log("FAIL: Could not open framebuffer");
            return 1;
        }
    };
    environment::log("PASS: Opened framebuffer");

    // Get surface info
    let info = match surface.info() {
        Ok(i) => i,
        Err(_) => {
            environment::log("FAIL: Could not get surface info");
            return 1;
        }
    };
    environment::log("PASS: Got surface info");

    // Draw a simple test pattern:
    // - Red rectangle at top-left
    // - Green rectangle at top-right
    // - Blue rectangle at bottom-left
    // - White rectangle at bottom-right
    let half_width = info.width / 2;
    let half_height = info.height / 2;

    // Red (top-left)
    let _ = surface.fill(Rect::new(0, 0, half_width, half_height), Colour::RED);

    // Green (top-right)
    let _ = surface.fill(Rect::new(half_width, 0, half_width, half_height), Colour::GREEN);

    // Blue (bottom-left)
    let _ = surface.fill(Rect::new(0, half_height, half_width, half_height), Colour::BLUE);

    // White (bottom-right)
    let _ = surface.fill(Rect::new(half_width, half_height, half_width, half_height), Colour::WHITE);

    // Flush to display
    let _ = surface.flush();

    environment::log("PASS: Drew test pattern");

    // Signal that screenshot can be taken
    environment::screenshot_ready();

    // Loop forever - test runner will capture screenshot and quit QEMU
    // (Can't use hlt in userspace - would cause General Protection Fault)
    loop {
        // Just spin
    }
}
