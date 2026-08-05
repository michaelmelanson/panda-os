#![no_std]
#![no_main]

//! Wire-protocol validation ground that the deleted `surface_test`/
//! `surface_overflow_test` used to cover through the raw `surface:` scheme
//! (plans/userspace-compositor.md, Phase 4): out-of-bounds damage rects,
//! oversized/undersized attach requests, and commits with no attached
//! buffer.
//!
//! This talks to a real compositor process (`compositor_test_child`) using
//! `compositor_protocol` directly rather than through `libpanda::graphics::
//! Window`, because `Window` validates blit geometry client-side before it
//! ever reaches the wire — exercising the compositor's own validation needs
//! requests `Window` wouldn't construct.
//!
//! What we found: none of these three cases are rejected with an error
//! event today, and one of them is worse than "not rejected" — it's a real
//! validation gap, not just a design choice:
//!
//! - `manager.rs` doesn't bounds-check `Damage` rects against the window's
//!   size at all; an out-of-bounds rect is accepted and just coalesced into
//!   the dirty region like any other.
//! - A `Commit` with nothing ever attached is a silent no-op (`manager.rs`:
//!   `if w.pending.is_none() && w.latched.is_none() { return }`) — no
//!   `FrameDone`, no error.
//! - `Attachment::new` *looks* like it rejects a buffer too small for its
//!   declared geometry (`needed = width*height*4; if len < needed { return
//!   None }`), which is Risk 2 of plans/userspace-compositor.md ("verify
//!   size ≥ w×h×4 ... before latching"). But `server.rs::map_attachment`
//!   passes `len = width*height*4` — the same declared-size computation,
//!   not the buffer's actual size — so that check always compares a number
//!   to itself and can never fail on the declared-size axis. `OP_BUFFER_MAP`
//!   (the kernel operation backing this) only returns an address, never a
//!   size, so there is currently no way for userspace to learn a mapped
//!   buffer's real size at all; closing this gap needs a kernel change
//!   (extending `OP_BUFFER_MAP` or adding a size query), which is out of
//!   scope for this phase's "don't touch panda-kernel" constraint. Until
//!   then, an under-allocated client buffer attached with an inflated
//!   declared size is a real out-of-bounds read/write hazard in the
//!   compositor, not just an accepted-but-harmless request. Because the
//!   actual outcome depends on how much real memory happens to back the
//!   mapping past the requested size (page rounding, allocator state), this
//!   test doesn't assert a specific result for that one commit — only that
//!   sending it doesn't crash or wedge the connection.
//!
//! None of the three crash or wedge the connection, which is what this test
//! actually asserts — there is no protocol-level rejection to test for one
//! way or the other, so each case is proven safe-to-send by showing the
//! compositor keeps answering requests normally afterwards.

use compositor_protocol::{Event, FORMAT_BGRA8888, MAX_FRAME_SIZE, Rect, Request};
use libpanda::buffer::Buffer;
use libpanda::environment;
use libpanda::ipc::Channel;
use libpanda::process;

fn send(channel: &Channel, request: Request) -> bool {
    let mut frame = [0u8; MAX_FRAME_SIZE];
    let Some(len) = request.encode(&mut frame) else {
        return false;
    };
    channel.send(&frame[..len]).is_ok()
}

fn send_with_handle(channel: &Channel, request: Request, handle: libpanda::Handle) -> bool {
    let mut frame = [0u8; MAX_FRAME_SIZE];
    let Some(len) = request.encode(&mut frame) else {
        return false;
    };
    channel.send_with_handle(&frame[..len], handle).is_ok()
}

/// Block until `WindowCreated` arrives, returning its window id.
fn expect_window_created(channel: &Channel) -> Option<u64> {
    let mut frame = [0u8; MAX_FRAME_SIZE];
    let len = channel.recv(&mut frame).ok()?;
    match Event::decode(&frame[..len]) {
        Some(Event::WindowCreated { window }) => Some(window),
        _ => None,
    }
}

/// Poll (non-blocking, bounded) for a `FrameDone` on `window`. Returns
/// whether one arrived within the poll budget.
///
/// A plain `yield_now()` spin isn't enough here: the compositor's frame
/// loop paces itself with a real `process::sleep(REFRESH_INTERVAL_MS)`
/// between ticks (server.rs), not a cooperative yield — so a busy-yielding
/// waiter with nothing else runnable just spins through its whole budget
/// before real time (and the compositor's timer) ever advances. Sleeping a
/// little between checks lets the compositor's own sleep actually elapse.
fn poll_for_frame_done(channel: &Channel, window: u64) -> bool {
    let mut frame = [0u8; MAX_FRAME_SIZE];
    // Comfortably more than one compositor tick (16 ms): enough attempts at
    // 4 ms apart to span several ticks even under host scheduling jitter.
    const POLL_INTERVAL_MS: u64 = 4;
    const POLL_ATTEMPTS: u32 = 64;
    for _ in 0..POLL_ATTEMPTS {
        if let Ok(Some(len)) = channel.try_recv(&mut frame) {
            if let Some(Event::FrameDone { window: w, .. }) = Event::decode(&frame[..len]) {
                if w == window {
                    return true;
                }
            }
        }
        process::sleep(POLL_INTERVAL_MS);
    }
    false
}

libpanda::main! {
    environment::log("Compositor protocol test starting");

    let Ok(compositor_handle) = environment::spawn("file:/initrd/compositor_test_child") else {
        environment::log("FAIL: could not spawn compositor_test_child");
        return 1;
    };
    let Some(channel) = Channel::from_handle_borrowed(compositor_handle) else {
        environment::log("FAIL: compositor handle is not a channel");
        return 1;
    };

    // Greeting.
    let mut frame = [0u8; MAX_FRAME_SIZE];
    let Ok(len) = channel.recv(&mut frame) else {
        environment::log("FAIL: no DisplayFormats greeting");
        return 1;
    };
    if !matches!(Event::decode(&frame[..len]), Some(Event::DisplayFormats { .. })) {
        environment::log("FAIL: greeting was not DisplayFormats");
        return 1;
    }
    environment::log("PASS: Connected to compositor");

    // ---- Case 1: commit with no attached buffer is a no-op, not a crash.
    if !send(&channel, Request::CreateWindow) {
        environment::log("FAIL: could not send CreateWindow");
        return 1;
    }
    let Some(window) = expect_window_created(&channel) else {
        environment::log("FAIL: did not receive WindowCreated");
        return 1;
    };
    if !send(&channel, Request::Commit { window }) {
        environment::log("FAIL: could not send Commit");
        return 1;
    }
    if poll_for_frame_done(&channel, window) {
        environment::log("FAIL: FrameDone arrived for a commit with nothing attached");
        return 1;
    }
    environment::log("PASS: Commit with no attached buffer produced no FrameDone");

    // ---- Case 2: an out-of-bounds damage rect doesn't crash the
    // compositor or wedge the connection — proven by a subsequent valid
    // commit still working normally.
    if !send(
        &channel,
        Request::Damage {
            window,
            rect: Rect { x: 1_000_000, y: 1_000_000, width: 1_000_000, height: 1_000_000 },
        },
    ) {
        environment::log("FAIL: could not send out-of-bounds Damage");
        return 1;
    }
    environment::log("PASS: Sent an out-of-bounds damage rect");

    // ---- Case 3: an attach whose buffer is too small for its declared
    // geometry. As the module doc explains, this is NOT reliably rejected
    // — `map_attachment` validates the declared size against itself, not
    // against the buffer's real size, because `OP_BUFFER_MAP` has no way to
    // report it. Whether the compositor happens to latch it and produce a
    // `FrameDone` isn't something this test can pin down (it depends on
    // the real page-backed size behind the mapping, which nothing here
    // controls), so — deliberately — this doesn't assert either outcome
    // for THIS commit. It only proves the request didn't crash or wedge
    // the connection, via the guaranteed-valid commit right after it.
    let Some(small_buffer) = Buffer::alloc(64) else {
        environment::log("FAIL: could not allocate small buffer");
        return 1;
    };
    if !send_with_handle(
        &channel,
        Request::AttachBuffer {
            window,
            width: 100,
            height: 100,
            format: FORMAT_BGRA8888,
        },
        small_buffer.handle(),
    ) {
        environment::log("FAIL: could not send undersized AttachBuffer");
        return 1;
    }
    if !send(&channel, Request::Commit { window }) {
        environment::log("FAIL: could not send Commit after undersized attach");
        return 1;
    }
    environment::log("PASS: Sent an undersized buffer attach and commit");

    // ---- The connection is still healthy: a normal attach + commit still
    // produces FrameDone after all three cases above.
    let Some(good_buffer) = Buffer::alloc(100 * 100 * 4) else {
        environment::log("FAIL: could not allocate a correctly sized buffer");
        return 1;
    };
    if !send_with_handle(
        &channel,
        Request::AttachBuffer {
            window,
            width: 100,
            height: 100,
            format: FORMAT_BGRA8888,
        },
        good_buffer.handle(),
    ) {
        environment::log("FAIL: could not send valid AttachBuffer");
        return 1;
    }
    if !send(&channel, Request::Commit { window }) {
        environment::log("FAIL: could not send valid Commit");
        return 1;
    }
    if !poll_for_frame_done(&channel, window) {
        environment::log("FAIL: no FrameDone after a valid attach and commit");
        return 1;
    }
    environment::log("PASS: Compositor still answers requests normally after all three cases");

    let exit_code = process::wait(compositor_handle);
    if exit_code != 0 {
        environment::log("FAIL: compositor_test_child exited with non-zero code");
        return 1;
    }

    0
}
