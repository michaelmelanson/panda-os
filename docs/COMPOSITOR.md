# Userspace compositor

The compositor (`userspace/compositor/`) owns the display exclusively and
composites client windows into the framebuffer. It replaces the in-kernel
compositor deleted in Phase 5 of `plans/userspace-compositor.md`; see that
plan for the full design rationale and history.

## Startup

`init` spawns the compositor as an independent sibling process, before any
graphical client:

1. It opens `display:/pci/display/0` (see `docs/SYSCALLS.md` "Display
   operations"), which claims the display exclusively via the kernel's claim
   table, and maps the framebuffer.
2. It registers the `compositor:` scheme (`OP_SCHEME_REGISTER`, see
   `docs/DEVICE_PATHS.md` and `docs/IPC.md` "Scheme provider protocol") so
   that clients — spawned independently by `init`, not as children of the
   compositor — can reach it by name via `environment::connect`.
3. It enters a ~16 ms tick loop: accept new client connections, apply
   pending client requests, composite damaged regions, flush them to the
   display.

If `display:` cannot be claimed (no display device, or something else holds
it), the compositor still comes up and serves windows over the protocol, it
just skips the flush step. This keeps headless test runs working without a
display.

## Client discovery and connection

A client reaches the compositor with `environment::connect("compositor:/connect")`,
which returns a `Channel` to a private conversation with the compositor (see
`docs/IPC.md`, "Scheme provider protocol" and `OP_ENVIRONMENT_CONNECT`). The
compositor greets every new connection with `DisplayFormats` (supported
pixel formats — `FORMAT_BGRA8888` today — and stride constraints).

## Protocol

The wire protocol is defined in `userspace/compositor-protocol/` — a
hand-rolled little-endian binary format in the same style as
`panda_abi::scheme_protocol`, one tag byte followed by fixed-width fields.
It is not part of `panda-abi`, since the kernel does not implement it.

```
client -> compositor:
  CreateWindow                        -> window id
  AttachBuffer{window, w, h, format}  (buffer handle attached to the message)
  Damage{window, rect}                (window-relative)
  Commit{window}
  Fill{window, rect, colour}
  SetVisible{window, bool}
  Move{window, x, y}
  DestroyWindow{window}

compositor -> client:
  DisplayFormats{formats, ...}        (on connect)
  WindowCreated{window}
  FrameDone{window, frame}            (after a Commit is consumed)
  BufferReleased{window, buffer}      (compositor no longer reads the buffer)
  Closed{window}
```

Buffer pixels never travel over the channel — only a handle to a
client-allocated `SharedBuffer`, attached to the `AttachBuffer` message (see
`docs/IPC.md` "Handle transfer") and mapped by the compositor with
`OP_BUFFER_MAP`.

## Buffer attach/latch/release lifecycle

Each window has at most one *pending* buffer (attached but not yet
committed) and one *latched* buffer (the one the compositor currently reads
from when compositing):

1. `AttachBuffer` maps the incoming buffer and stores it as the window's
   pending attachment, replacing any previous pending attachment.
2. `Commit` latches the pending attachment: it becomes the new latched
   buffer, and the *previous* latched buffer (if any) is released —
   `BufferReleased{window, buffer}` is sent once the compositor will no
   longer read from it. A client that wants tear-free double buffering
   renders into buffer A, commits, and only reuses buffer A after seeing
   `BufferReleased{A}`; a single-buffered client just reuses one buffer and
   accepts the ordinary single-buffer semantics.
3. Damage rects accumulate against a window between commits; the tick that
   consumes a commit composites the accumulated damage and then sends
   `FrameDone{window, frame}`.
4. `Fill` writes directly into the latched content (not the pending one) —
   this is what makes `Window::fill`/`clear` in `libpanda::graphics` work
   with no kernel involvement, fixing a bug the in-kernel compositor never
   closed.

Buffers are refcounted by handle (`SharedBuffer`, see
`panda-kernel/src/resource/buffer.rs`): the compositor holds its own handle
to every attached buffer, so a client exiting or freeing a buffer mid-frame
can never invalidate memory the compositor is reading — it just leaves the
window stale until the compositor processes the client's disconnect.

## Client library

`libpanda::graphics::Window` (`userspace/libpanda/src/graphics/`) wraps the
protocol: `create`/`fill`/`blit`/`flush` allocate and manage one buffer by
default (`blit` copies into it and sends `Damage`; `flush` sends `Commit`
and waits for `FrameDone`), with an opt-in second buffer for tear-free
clients. The terminal (`userspace/terminal/`) is built on this library
rather than talking to the compositor protocol directly.

The alpha-blend implementation lives once, in `userspace/compositor-protocol/`
(`blend.rs`), shared by the compositor and the client library — the six
independent, disagreeing blend implementations the in-kernel compositor era
had (see `plans/userspace-compositor.md`, "Problem") are gone.
