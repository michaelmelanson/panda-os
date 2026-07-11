# Panda OS roadmap

This document records the decided end state of the system and the sequence of
milestones to get there. Individual `plans/*.md` documents cover the detailed
design of each milestone; this file is the map that orders them. When a decision
here changes, update this file first, then the affected plans.

Decisions recorded 2026-07-11.

## End state

1. **Strict microkernel.** The kernel provides scheduling, memory management,
   IPC (channels, mailboxes, shared buffers), and device ownership (claim
   tokens, IOMMU domains, MMIO/DMA/IRQ syscalls). Everything else — device
   drivers, the compositor, filesystems, the network stack — runs as userspace
   services.
2. **Typed schemes are the permanent naming model** (Redox-style).
   `scheme:path` URIs stay first-class. Userspace services register schemes and
   the kernel routes resource operations on those schemes to the provider
   process. A real scheme registry with an enumeration protocol replaces the
   current `*:` discovery special case. There is no unified single tree;
   `file:` is one scheme among peers, served by a filesystem service.
3. **Capability-based security.** Handles are unforgeable and are the *only*
   authority: holding a handle is permission, delegation is handle transfer
   over a channel, and spawn grants a child a minimal explicit set. No user
   IDs, no ambient authority. The device-driver-model's claim tokens are the
   first instance of this pattern; scheme access becomes capability-gated in
   the same way.
4. **Universal device claim model.** One kernel mechanism arbitrates exclusive
   device ownership for every device class: the display, block devices (a
   mounted disk refuses raw opens), and eventually every claimable device via
   `OP_DEVICE_CLAIM`.
5. **Service-structured graphics.** A display driver service claims the GPU and
   registers `display:`; the compositor is a *client* of `display:` and serves
   windows to apps; an input driver service registers the input scheme and the
   compositor routes events by focus. Window pixel buffers are
   **client-allocated** (Wayland-style attach), enabling arbitrary buffer
   counts and future GPU-rendered client buffers.
6. **Long-horizon capabilities**, in rough order: SMP, TCP/IP networking, a
   POSIX compatibility layer, and ultimately self-hosting development (Panda
   building Panda).

### End-state process architecture

```
 kernel: scheduler | memory | IPC | scheme routing | device claims + IOMMU
 ───────────────────────────────────────────────────────────────────────
 userspace services (each claims its device, registers its scheme):
   display driver  → display:      input driver → input:
   block driver    → block:        net driver   → (used by net service)
   fs service      → file:         net service  → tcp:, udp:
   compositor      → window:   (client of display: and input:)
 apps: terminal, shell tools → window:, file:, tcp: ...
 init/service manager: spawns, supervises, grants capabilities
```

## Milestones

Each milestone leaves `make test` green and the docs matching the tree. Order
matters: the rationale for the sequence is at the end.

### M0 — quality baseline (no new features)

The refactoring pass already scoped in review: delete dead code
(`ProcessResource`, unreachable `Resource` downcasts, `initrd.rs` duplicate TAR
parser, `*:` discovery, dead device-path/pci/vfs code), extract the duplicated
syscall helpers (VFS-file resolution, directory-path resolution, blocking
skeleton), consolidate ext2 directory walking, and fix lossy error conversions.

Memory subsystem in M0: **keep the recursive-mapping + RAII design** — the
physical-memory window was built and deliberately removed (`a2fa8eb`, 2026-01)
because it aliased every physical byte at a second kernel vaddr and caused a
DMA double-mapping bug; the current design (heap-backed `Frame`s with a single
vaddr each, recursive walks for the active address space, RAII
`PhysicalMapping` for MMIO) is the decided architecture. The M0 work is to
**unify the three page-table walkers** behind one implementation parameterized
over table access (recursive window for the current address space; owned frame
vaddrs for an address space under construction), and to share the
unmap/free-empty-tables logic between `paging.rs` and `demand_paging.rs`.

Finish with a docs truth pass: rewrite `docs/HIGHER_HALF_KERNEL.md` to
describe the implemented design and record *why* the physmap was removed —
its staleness is what led this very roadmap to briefly re-propose the physmap
— plus DEVICE_PATHS and VFS corrections.

### M1 — IPC and ownership primitives

The two general-purpose mechanisms every later milestone builds on, plus the
first two applications of the claim model:

- **Handle transfer over channels** (SCM_RIGHTS analogue) — required for
  buffer sharing (M3), capability delegation (M6), and service bootstrap.
- **Multi-process shared buffers** — `SharedBuffer` becomes frames + per-process
  mappings (`OP_BUFFER_MAP`); frames freed when the last handle drops.
- **Claim table** — exclusive ownership keyed by device address: the display
  claims first (`Busy` on second open), then block devices (mounting a disk
  claims it; raw `block:` opens on a mounted device fail).

### M2 — userspace scheme providers (the keystone)

The primitive that makes a strict microkernel possible: a process registers a
scheme (`OP_SCHEME_REGISTER("display")` → provider handle) and the kernel
routes `open`/`read`/`write`/`readdir`/`close` on that scheme to the provider
over channel-backed IPC, correlating requests and responses. Includes:

- The scheme registry with a real enumeration protocol (the replacement for
  the removed `*:`).
- Re-scope `plans/system-init-tool.md` Phase 3: services register *schemes*,
  not names — `service:/keyboard` from the driver plan becomes the driver
  registering an input scheme directly.
- Kernel-side schemes (`file:`, `display:`, …) and userspace-provided schemes
  are indistinguishable to clients, which is what lets providers migrate out
  of the kernel one at a time in M3–M5 without breaking callers.

### M3 — graphics to userspace

`plans/userspace-compositor.md`. Interim kernel `display:` resource (thin:
info/map/flush, exclusively claimed) → compositor process with the
client-allocated-buffer protocol → libpanda `Window` rewritten over it →
terminal and window tests ported → kernel compositor, window resource, and
surface syscalls deleted (~1,500 lines). The `display:` *interface* designed
here is permanent; only its provider moves in M4.

### M4 — userspace drivers and IOMMU

`plans/iommu.md` + `plans/device-driver-model.md`, with two additions from the
end-state decisions:

- The **display driver service** ports the kernel virtio-gpu driver to
  userspace: it claims the GPU, registers `display:` (same interface as M3's
  kernel version), and the kernel's last graphics code is deleted.
- The **input driver service** (virtio-keyboard, the driver plan's Phase 7)
  registers the input scheme; the compositor subscribes and routes key events
  to the focused window. Apps stop opening input devices directly.
- The **block driver service** ports virtio-blk, registering `block:` under
  the claim rules from M1.

### M5 — filesystems to userspace

The filesystem service takes the kernel's VFS policy and ext2 implementation
and registers `file:`; it is a client of `block:`. The kernel retains only a
read-only `initrd:` scheme (in-memory ustar, already specified in the driver
plan) to bootstrap init, the service manager, and the fs service itself.
Kernel `vfs/` is deleted. This completes the strict-microkernel migration.

### M6 — capability enforcement

Flip the default from "any process may open any scheme" to explicit grants:

- Per-process scheme capability sets, granted at spawn by the parent/service
  manager; opening a scheme you hold no capability for fails.
- Device observation capabilities gate `OP_DEVICE_SUBSCRIBE` (the slot the
  driver plan already reserved).
- Audit and remove remaining ambient authority (e.g. `HANDLE_ENVIRONMENT`'s
  open-anything power becomes a capability like any other).

Structurally this is mostly *enabling checks at existing choke points* — the
handle model, tokens, and transfer primitive from M1 are the machinery.

### M7 — SMP

Deliberately after M3–M5: by now the kernel is small (no compositor, no
filesystems, no drivers), so the surface to parallelise is minimal. Work:
per-CPU run queues and state, IPIs, TLB shootdown (including invalidation of
recursive-window entries when page tables change — the one SMP-specific cost
of the recursive-mapping design), unifying the mutually-recursive
scheduler/executor pair into one layered design, and re-auditing every
spinlock and `Waker` under concurrency.

### M8 — networking

Virtio-net driver service (device model from M4) plus a TCP/IP service
registering `tcp:`/`udp:` (evaluate porting smoltcp before writing a stack),
and a socket-style client API in libpanda.

### M9 — POSIX compatibility layer

A libc translation layer over schemes: path translation into `file:`, fd table
over handles, `fork`/`exec` emulation strategy (likely posix_spawn-shaped),
pipes over channels. Drives requirements back into M5's fs service semantics
(permissions metadata, `O_*` flags) and M8's sockets. Scope: port existing C
programs, starting with shell utilities.

### M10 — self-hosting (north star)

Toolchain and editor on Panda, building Panda. Not planned in detail; it is
the forcing function for stability, POSIX coverage, and performance work in
M7–M9.

## Sequencing rationale

- **M0 before everything**: every later milestone edits the same kernel; the
  dedup and physmap work compound across all of them.
- **M2 before M3–M5**: userspace providers need request routing; designing the
  scheme interfaces first (`display:` in M3) means providers can relocate from
  kernel to userspace without clients noticing.
- **M6 after M4/M5**: capability gates are cheap to add but only meaningful
  once the things worth gating (devices, filesystems) are services with
  explicit boundaries.
- **M7 (SMP) after the kernel shrinks**: parallelising a 20k-line kernel that
  still contains a compositor and ext2 would mean redoing that work as each
  subsystem moves out.
- **M8–M10 last**: they consume the platform; they don't shape it.

## Existing plans mapped to milestones

| Plan | Milestone | Status |
|---|---|---|
| `plans/userspace-compositor.md` | M3 (interface), M4 (display service) | Active |
| `plans/iommu.md` | M4 | Active, unchanged |
| `plans/device-driver-model.md` | M4 | Active; service registration re-scoped by M2 |
| `plans/system-init-tool.md` | M2/M4 | Phase 3 re-scoped: schemes, not names |
| `plans/virtio-gpu-3d-composition.md` | M4+ | Superseded as written; re-scope with the display service as the GPU client |
