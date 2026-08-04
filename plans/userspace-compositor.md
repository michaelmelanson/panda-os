# Userspace compositor

Milestone M3 of `plans/ROADMAP.md`; the display driver service that completes it
is milestone M4.

## Decision

Compositing moves out of the kernel into a userspace compositor process. The
framebuffer is **exclusively owned**: exactly one process holds the display at a
time, enforced by the kernel's claim mechanism. All window management, damage
tracking, and pixel blending happens in userspace.

Per the roadmap's end-state decisions:

- The compositor is a **client of the `display:` scheme**, never the GPU driver.
  In this plan the kernel provides `display:` (a thin resource: mode info,
  framebuffer mapping, flush). In M4 a userspace display driver service claims
  the GPU and registers the *same* `display:` interface; the compositor doesn't
  change.
- Window pixel buffers are **client-allocated** (Wayland-style): clients create
  shared buffers and attach them to windows. This supports arbitrary buffer
  counts (single-, double-, or triple-buffered clients) and future GPU-rendered
  client buffers without protocol changes.
- Input routing is ultimately owned by a **separate input driver service** (M4);
  the compositor subscribes to it and routes events by focus. Until then, apps
  keep opening `keyboard:` directly.
- Display exclusivity is the first application of the **universal claim model**
  (roadmap decision 4); block devices follow in M1 of the roadmap.

## Problem

The kernel currently contains a full compositor (`panda-kernel/src/compositor/mod.rs`,
~470 lines): a 60 fps kernel task, per-window pixel buffers owned by the kernel,
damage coalescing, and per-pixel alpha blending. Window blit logic lives in the
syscall layer (`syscall/surface.rs`, 662 lines). Known consequences:

1. **Double copy on every frame**: client pixels are copied userspace → kernel
   `Window::pixel_data` (blit syscall) → framebuffer (compositor tick).
2. **Policy in the kernel**: blend formulas, opacity fast paths, dirty-rect
   coalescing, background colour, and frame pacing are all kernel code,
   duplicated and diverging (six independent alpha-blend implementations across
   kernel and userspace; the syscall path and `alpha_blend()` disagree on
   output alpha).
3. **Broken and unsound edges**: `fill`/`clear` on a window always fails
   (`handle_fill` never grew the window special case); `open("surface:/fb0")`
   mints unsynchronized aliases of the framebuffer pointer under an unjustified
   `unsafe impl Send/Sync` while the compositor writes the same memory.
4. **Contradicts the microkernel end state** (roadmap decision 1).

## Goals

1. The kernel contains no compositing logic: no window concept, no blending, no
   damage tracking, no frame pacing.
2. Exactly one process can hold the display at a time, enforced by the kernel.
   `surface:/fb0` and its aliasing hazard are retired.
3. Zero kernel copies on the pixel path: clients render into buffers they
   allocate and the compositor maps; the compositor blends straight into the
   mapped framebuffer.
4. One alpha-blend implementation, in userspace, shared by the compositor and
   libpanda.
5. The client API (`libpanda::graphics::Window`) survives mostly unchanged, and
   the terminal is ported onto it — ending the situation where the only real
   GUI program bypasses the library that only tests use.
6. The window `fill`/`clear` path works (it becomes an ordinary protocol
   request).
7. The `display:` interface designed here survives unchanged when its provider
   moves to userspace in M4.

## Design

### Kernel primitives (roadmap M1, prerequisites)

Two general-purpose mechanisms, specified here because this plan is their first
consumer:

**Handle transfer over channels.** A channel message may carry one attached
handle (SCM_RIGHTS analogue). `OP_CHANNEL_SEND` gains a handle argument (0 =
none); on receive, the kernel installs the resource into the receiver's handle
table and returns the new handle id alongside the payload. Clients use this to
send their window buffers to the compositor; `init` uses it to bootstrap the
compositor↔client connection before userspace scheme routing (roadmap M2)
exists. Transferable resource types are whitelisted initially: `SharedBuffer`
and `ChannelEndpoint`.

**Cross-process buffer mapping.** `SharedBuffer` currently maps into exactly one
address space at allocation time. It is restructured so the physical frames are
the buffer's identity and mappings are per-process: a received buffer handle is
mapped into the receiver with `OP_BUFFER_MAP(handle) → vaddr`, and each
process's mapping is torn down when its handle closes. The frames are freed when
the last handle drops.

### The display resource

A new `display:` scheme replaces `surface:/fb0` and `surface:/window`:

| Operation | Behaviour |
|---|---|
| `open("display:/pci/display/0")` | Claims the display **exclusively** via the M1 claim table. A second open anywhere returns `Busy`. Close (or process exit) releases the claim. |
| `OP_DISPLAY_INFO` | Returns width, height, stride, pixel format. |
| `OP_DISPLAY_MAP` | Maps the framebuffer pages into the caller's address space; returns vaddr. Reuses the per-process mapping machinery above. |
| `OP_DISPLAY_FLUSH(rect)` | Forwards the damaged rect to the driver (virtio-gpu transfer+flush). Async; completes when the device acknowledges. |
| `EVENT_DISPLAY_CHANGED` | Posted to the owner's mailbox on mode/resolution change; owner re-queries INFO and re-maps. Replaces `compositor::replace_framebuffer`. |

This table is the permanent `display:` contract. In this milestone the provider
is ~300 lines of kernel plumbing in front of the virtio-gpu driver; in M4 the
provider becomes the userspace display driver service and the kernel lines are
deleted.

`FramebufferSurface`'s `unsafe impl Send/Sync` becomes legitimate in the
interim: exactly one owner exists, and the kernel driver touches the
framebuffer only inside `OP_DISPLAY_FLUSH` on behalf of that owner.

### The compositor process

`userspace/compositor/` — spawned by `init` before any graphical client. Core
loop:

1. Claim `display:/pci/display/0`, map the framebuffer.
2. Accept client connections (via the scheme registry once roadmap M2 lands;
   via an `init`-provided channel until then). Greet each client with
   `DisplayFormats` (supported pixel formats — BGRA initially — and any stride
   constraints).
3. Track per-window state: position, size, visibility, z-order, the currently
   attached buffer (a mapped client `SharedBuffer`), and pending damage.
4. On `Commit`: latch the attached buffer and accumulated damage for the next
   composite; send `FrameDone{frame}` after the tick that consumes it.
5. Composite on a ~16 ms tick, exactly as the kernel does today — clear damaged
   screen regions to the background, blend windows back-to-front with the
   opaque-region row-copy fast path — then `OP_DISPLAY_FLUSH` each region. The
   `WindowManager` logic (`compositor/mod.rs:49-237`) ports over nearly
   verbatim; this is a move, not a rewrite.

### Protocol

The wire protocol lives in a new userspace crate,
`userspace/compositor-protocol/` — **not** in `panda-abi`, which stays reserved
for what the kernel actually implements. Messages (all ≤ 4 KB channel limit;
buffers travel as attached handles, never as payload):

```
client → compositor:
  CreateWindow                        → window id
  AttachBuffer{window, w, h, format}  (buffer handle attached to the message)
  Damage{window, rect}                (window-relative)
  Commit{window}
  Fill{window, rect, colour}
  SetVisible{window, bool}
  Move{window, x, y}
  DestroyWindow{window}

compositor → client:
  DisplayFormats{formats, ...}        (on connect)
  WindowCreated{window}
  FrameDone{window, frame}            (after a Commit is consumed)
  BufferReleased{window, buffer}      (compositor no longer reads the buffer)
  Closed{window}
```

Client-allocated buffers make the synchronization model explicit rather than
implicit: a client that wants tear-free output attaches buffer B, commits, and
renders the next frame into buffer A only after `BufferReleased{A}`; a
single-buffered client simply reuses one buffer and accepts today's semantics.
Resize is `AttachBuffer` with new dimensions — no separate message or
kernel-side realloc dance.

`Fill` is handled compositor-side (write into the latched content) — this is
what makes the currently-broken `Window::fill`/`clear` work again with no
kernel involvement.

### Client library

`libpanda::graphics::Window` keeps its public shape (create/fill/blit/flush)
but is reimplemented over the protocol: it allocates one buffer by default
(`blit` = memcpy into it + `Damage`; `flush` = `Commit` + wait for
`FrameDone`), with an opt-in second buffer for tear-free clients. The terminal
is ported onto it, deleting its raw `sys::surface` calls and its private
glyph-blend loop. One blend implementation remains, in `compositor-protocol`
(or a small `panda-pixels` crate), used by the compositor, `PixelBuffer`, and
the terminal.

### What gets deleted from the kernel

| Code | Size |
|---|---|
| `compositor/` (task, WindowManager, frame waiters) | ~470 lines |
| `syscall/surface.rs` (all of it — blit/fill/flush/update_params) | ~660 lines |
| `resource/window.rs` + `WindowResource` stub `Surface` impl | ~80 lines |
| `resource/surface.rs`: `Surface` trait, `alpha_blend`, `get_framebuffer_surface`, aliasing `Send/Sync` | ~250 of 427 lines |
| `surface:` scheme entries (`/fb0`, `/window`), `as_window`/`as_surface`/`as_surface_mut` on `Resource` | ~100 lines |

Net: roughly 1,500 kernel lines of policy removed, replaced by ~300 lines of
interim display-resource plumbing (deleted in turn by M4) plus the two
general-purpose M1 primitives.

## Implementation plan

### Phase 1: handle transfer + multi-process buffers (roadmap M1) — ✅ landed

`1194f0b` (handle transfer), `4d8c098` (multi-process buffers/`OP_BUFFER_MAP`).

- `resource/channel.rs`: message struct gains `Option<Arc<dyn Resource>>`;
  send-side validation (whitelist), receive-side handle-table installation.
- `resource/buffer.rs`: split `SharedBuffer` into frames (shared identity) +
  per-process `BufferMapping`; add `OP_BUFFER_MAP`; close/exit tears down only
  the closing process's mapping.
- ABI: extend `OP_CHANNEL_SEND`/`OP_CHANNEL_RECV` register conventions
  (`docs/SYSCALLS.md`), add `OP_BUFFER_MAP`.
- Tests landed as userspace integration tests rather than kernel-only tests —
  `handle_transfer_test`/`_child`, `buffer_transfer_test`/`_child`, and (from
  a follow-up ownership-bug fix, `b4dba11`/`dcf6986`) `buffer_owner_test`/
  `_child`, which is the regression coverage for a real bug this phase's
  design left open: a process holding a *transferred-but-unmapped* buffer
  handle could reach `read`/`write`/`resize`/`free` and corrupt memory in the
  wrong address space. Fixed and covered before Phase 2 started, not a
  follow-up debt against this phase.

### Phase 2: exclusive display resource — ✅ landed

`358469a`.

- `display:` scheme with the claim table; `Busy` on second open; release on
  close/exit.
- `OP_DISPLAY_INFO` / `OP_DISPLAY_MAP` / `OP_DISPLAY_FLUSH`;
  `EVENT_DISPLAY_CHANGED` wired from the virtio-gpu resolution-change path.
- **Deviation from the plan as written**: the kernel compositor claims the
  display *permanently* (at `compositor::init`, using the same
  `ClaimOwner::Display` that legacy `surface:/fb0` already used), not merely
  "keeps running so nothing breaks" — this was the only sound resolution to a
  problem the original plan text didn't anticipate: without a permanent
  claim, a userspace `display:` open could succeed *while the compositor is
  still writing the same framebuffer memory unsynchronized*, recreating
  exactly the hazard this phase exists to close. The consequence: `/fb0` is
  now *always* `Busy` for as long as the kernel compositor exists, which is
  the entire span of Phases 2–4. `surface_test`/`surface_overflow_test` were
  rewritten to assert `Busy` rather than draw to the framebuffer — acceptable
  because `/fb0` is deleted outright in Phase 4/5 regardless, and real
  pixel-output coverage already lives in the compositor-path screenshot
  tests (`window_test`, `alpha_test`, `multi_window_test`,
  `partial_refresh_test`, `window_move_test`).
- **Test coverage is narrower than "two opens → Busy; owner exit → reopen
  succeeds; flush round-trip"** as originally planned, precisely because of
  the point above: nothing *but* the kernel compositor can ever hold the
  claim during this phase, so "owner exit → reopen succeeds" and a real
  `OP_DISPLAY_MAP`/`OP_DISPLAY_FLUSH` round trip are not exercisable until
  Phase 5 deletes the kernel compositor. `display_test` (new) covers what
  is exercisable now: exclusivity (`Busy` on every open attempt, through
  both `display:` and `surface:/fb0`), path resolution (`NotFound` for a
  nonexistent display index and for a non-display device), and that
  `OP_DISPLAY_INFO`/`MAP`/`FLUSH` reject a non-display handle rather than
  crashing. The full round trip is carried forward as explicit, tracked debt
  against Phase 5, not silently dropped.

### Phase 3: compositor process + protocol crate

- `userspace/compositor-protocol/`: message types, encode/decode, the single
  `alpha_blend` + `is_region_opaque` implementation (ported from kernel, with
  the output-alpha divergence resolved deliberately — pick the
  `src_a + dst_a·(1−src_a)` form and document it).
- `userspace/compositor/`: port `WindowManager` composite/damage logic; client
  connection handling; buffer attach/latch/release lifecycle; `init` spawns it
  and hands clients a channel to it.

### Phase 4: port clients

- Rewrite `libpanda::graphics::Window` over the protocol.
- Port the terminal (delete its raw surface calls and private blend loop).
- Port `window_test`, `multi_window_test`, `alpha_test`,
  `partial_refresh_test`, `window_move_test`, `size_cap_test`'s surface cases.
  `surface_test` and `surface_overflow_test` (raw `/fb0` consumers) are
  replaced by a `compositor_protocol_test` covering the same validation ground
  (out-of-bounds damage rects, oversized attach requests, commits with no
  attached buffer).

### Phase 5: kernel deletion (atomic cutover)

- Delete the table above in one commit, following the device-driver-model
  precedent: no coexistence period.
- Update `docs/` (new `docs/COMPOSITOR.md`; prune `docs/DEVICE_PATHS.md`
  surface examples; update `docs/IPC.md` for handle transfer).

### Handed off to roadmap M4

- **Display driver service**: port the kernel virtio-gpu driver to a userspace
  service that claims the GPU and registers the same `display:` interface;
  delete the interim kernel provider.
- **Input service + focus routing**: the input driver service registers the
  input scheme; the compositor subscribes, tracks focus, and delivers events
  over each client's channel; apps stop opening `keyboard:` directly.
- **GPU-accelerated composition**: re-scope
  `plans/virtio-gpu-3d-composition.md` with the display service as the virgl
  client and the compositor issuing composition requests through `display:`.

## Testing

- Phase-by-phase as listed above; `make test` green at every phase boundary.
- End-to-end: existing window tests passing against the userspace compositor
  is the acceptance criterion for Phases 3–5, mirroring how `keyboard_test`
  validates the userspace keyboard driver cutover.
- New regression tests: window `fill`/`clear` actually renders (the bug this
  refactor incidentally fixes must not come back); `BufferReleased` ordering
  (compositor never reads a buffer after releasing it).

## Risks

1. **Test bootstrapping.** Userspace graphics tests now need the compositor
   running. `init` must spawn it before test programs, and a test-mode
   compositor must not require a display to exist (headless QEMU test runs):
   if `display:` open fails, the compositor still serves windows and skips
   flushes.
2. **Client-allocated buffer validation.** The compositor must treat attached
   buffers as untrusted: verify size ≥ w×h×4 for the declared format before
   latching, and handle a client resizing/freeing mid-frame. Frames are
   refcounted by handles (Phase 1) and the compositor holds its own handle to
   every attached buffer, so client exit can never invalidate memory under the
   compositor — it just makes the window stale until the close is processed.
3. **Channel throughput.** Damage/commit messages are small; pixel data never
   crosses the channel. The 4 KB message cap is not a constraint on this
   design.
4. **Frame pacing without a kernel tick.** The compositor sleeps via the
   existing timer syscalls; if sleep granularity proves too coarse for 16 ms
   pacing, `OP_DISPLAY_FLUSH` completion (device ack) can serve as the pacing
   signal instead.
5. **Resolution changes.** `EVENT_DISPLAY_CHANGED` + re-map replaces the
   kernel's `replace_framebuffer`; the compositor must treat the old mapping
   as dead before re-mapping. The kernel keeps the old framebuffer pages alive
   until the owner unmaps, so a racing flush hits stale-but-valid memory,
   never unmapped pages.
