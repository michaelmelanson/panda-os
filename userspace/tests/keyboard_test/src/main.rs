#![no_std]
#![no_main]

use libpanda::environment;
use libpanda::file;
use libpanda::ipc::Channel;
use libpanda::keyboard::{RawInputEvent, keycode_to_char};

const EV_KEY: u16 = 0x01;

libpanda::main! {
    environment::log("Keyboard test starting");

    // This test plays the role of `init` (it's the initrd's `init`
    // binary), so unlike the real boot path (where `init`'s driver
    // registry spawns every driver it finds under `initrd:/drivers`) there
    // is no service manager running to spawn the virtio-keyboard driver —
    // do it directly, the same way other userspace-scheme-provider tests
    // spawn their provider (see `userspace/tests/scheme_provider_test`).
    let Ok(driver_handle) = environment::spawn("initrd:/virtio-keyboard") else {
        environment::log("Could not spawn virtio-keyboard driver");
        return 1;
    };
    // Wait for the driver to actually claim the device and register
    // `keyboard:` before racing `environment::open` against its async
    // startup (device claim, feature negotiation, virtqueue setup) — see
    // the driver's "ready" signal in `userspace/drivers/virtio-keyboard`.
    let Some(driver_channel) = Channel::from_handle_borrowed(driver_handle.into()) else {
        environment::log("Driver handle is not a channel");
        return 1;
    };
    let mut ready_msg = [0u8; 64];
    if driver_channel.recv(&mut ready_msg).is_err() {
        environment::log("Did not receive ready signal from virtio-keyboard driver");
        return 1;
    }

    // Open the keyboard device at PCI 00:03.0
    let keyboard = if let Ok(h) = environment::open("keyboard:/pci/00:03.0", 0, 0) {
        h
    } else {
        environment::log("Could not open keyboard");
        return 1;
    };

    environment::log("Keyboard opened successfully!");
    environment::log("Waiting for key events (this will block)...");
    environment::log("Press keys to see events. Test will read 10 events then exit.");

    let mut event_buf = [0u8; 8]; // sizeof(RawInputEvent) = 8 bytes
    let mut events_read = 0;
    let mut msg_buf = [0u8; 32];

    while events_read < 10 {
        let n = file::read(keyboard, &mut event_buf);
        if n < 0 {
            environment::log("Error reading from keyboard");
            return 1;
        }
        if n as usize >= core::mem::size_of::<RawInputEvent>() {
            let event = unsafe { &*(event_buf.as_ptr() as *const RawInputEvent) };
            if event.event_type == EV_KEY {
                let key_char = keycode_to_char(event.code, false).unwrap_or('?');

                // Format message with key character
                let action = if event.value == 1 {
                    "pressed"
                } else if event.value == 0 {
                    "released"
                } else {
                    "repeat"
                };

                // Build message: "Key 'x' pressed" or similar
                let msg = format_key_msg(&mut msg_buf, key_char, action);
                environment::log(msg);
                events_read += 1;
            }
        }
    }

    file::close(keyboard);

    // The virtio-keyboard driver runs forever (it's a persistent driver
    // process in the real boot path — see `userspace/drivers/virtio-keyboard`'s
    // main loop), so unlike a normal child process it won't exit on its own.
    // The kernel only shuts QEMU down via isa-debug-exit once *no* processes
    // remain (`panda-kernel/src/scheduler/mod.rs`'s `exec_next_runnable`) —
    // this test plays `init`, so it must ask the driver to exit before
    // returning itself, or QEMU just runs until the harness's own timeout
    // even though the test already passed. There's no process-kill syscall
    // in this kernel (see `SpawnHandle::signal`'s doc comment for why one
    // isn't safe to add casually), so this is a cooperative request the
    // driver checks for on its own — see its main loop's handling of
    // `SHUTDOWN_REQUEST`.
    let _ = driver_channel.send(b"shutdown");

    environment::log("Keyboard test passed - received 10 key events");
    0
}

fn format_key_msg<'a>(buf: &'a mut [u8], key: char, action: &str) -> &'a str {
    // "Key 'x' pressed"
    let prefix = b"Key '";
    let mid = b"' ";

    let mut pos = 0;
    for &b in prefix {
        buf[pos] = b;
        pos += 1;
    }
    buf[pos] = key as u8;
    pos += 1;
    for &b in mid {
        buf[pos] = b;
        pos += 1;
    }
    for &b in action.as_bytes() {
        buf[pos] = b;
        pos += 1;
    }

    core::str::from_utf8(&buf[..pos]).unwrap_or("Key event")
}
