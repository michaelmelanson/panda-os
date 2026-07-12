# Higher-half kernel

The kernel runs entirely in the upper canonical half of the virtual address
space; userspace owns the entire lower half. This document describes the boot
transition that gets the kernel there and the memory-access design it runs
under. For the full address map, see
[VIRTUAL_ADDRESS_SPACE.md](VIRTUAL_ADDRESS_SPACE.md).

## Kernel regions

| Region | Base | Purpose |
|--------|------|---------|
| MMIO region | `0xffff_9000_0000_0000` | Device MMIO (PCI BARs, APIC, ACPI tables) |
| Kernel heap | `0xffff_a000_0000_0000` | All kernel allocations |
| Kernel image | `0xffff_c000_0000_0000` | Relocated kernel code and data |

Defined in `panda-kernel/src/memory/address_space.rs`. PML4 entry 510 is
reserved for the recursive page-table mapping (see below).

## Boot transition

The kernel is a UEFI PE/COFF application with position-dependent code: UEFI
loads it at an arbitrary physical address and identity-maps it. Boot
(`main.rs`, shared with the test harness via `boot::init_and_jump` /
`boot::init_higher_half` in `panda-kernel/src/boot.rs`) performs:

1. **Early init** (`panda_kernel::init()`): memory subsystem, early frame
   allocator, and heap mapping via raw page-table walks (the recursive
   mapping does not exist yet) in `address_space.rs`.
2. **Kernel relocation**: the kernel's physical pages are mapped a second
   time at `KERNEL_IMAGE_BASE`, and the PE `.reloc` section (parsed with
   `goblin`) is applied to the higher-half copy — each `DIR64` entry gets
   `new_base - image_base` added. The kernel is briefly dual-mapped.
3. **Stack switch and jump**: data that must survive the jump is stashed in
   statics (`AtomicU64`), RSP is switched to the higher-half address of a
   static kernel stack, and execution jumps to a higher-half continuation.
4. **Continuation**: reloads the stashed values, finishes initialization
   (ACPI, interrupts, syscalls), translates any stashed pointers to
   higher-half or mapped addresses, then **removes the identity mapping**.
   From this point nothing below `0xffff_8000_0000_0000` is mapped in the
   kernel's address space except per-process userspace.

## Physical memory access

There is deliberately **no physical-memory window** (direct map). Each
physical byte the kernel can touch has exactly one kernel virtual address:

- **Frames come from the heap.** `memory::allocate_frame()` allocates from
  the kernel heap (zeroed) and records both the physical frame and the heap
  virtual address in the RAII `Frame` guard (`memory/frame.rs`). CPU access
  to a frame's contents goes through `Frame::virtual_address()`.
- **Page tables are walked via the recursive mapping.** PML4[510] points at
  the PML4 itself (`memory/recursive.rs`), which makes every page table of
  the *active* address space addressable at a computable virtual address.
  Runtime mapping/unmapping (`memory/paging.rs`) and demand paging use this.
  Early-boot and address-space-construction walks instead use the owned
  frames' addresses directly (`address_space.rs::get_or_create_table`),
  since they operate on tables that are not the active address space.
- **MMIO and foreign physical ranges** use the RAII `PhysicalMapping`
  (`memory/mmio.rs`), which allocates a virtual range in the MMIO region,
  maps it uncacheable as needed, and unmaps on drop. The vaddr allocator
  supports deallocation with coalescing.

### Design decision: the physical window was removed

An explicit physical-memory window at `0xffff_8000_0000_0000` was built
during the original higher-half migration and then **deliberately removed**
in commit `a2fa8eb` ("Remove physical memory window, use recursive page
tables"). The window meant every physical byte was reachable through two
virtual addresses — its window address and its heap/MMIO mapping — and that
aliasing produced a real bug (DMA buffers accessible via two vaddrs, with
attendant cache-attribute hazards).

The current design (heap-backed frames + recursive mapping + RAII
`PhysicalMapping`) keeps a single vaddr per physical byte. Do not reintroduce
a direct map without revisiting that decision: the known trade-off is that
usable physical memory is bounded by what the kernel heap covers, which is a
non-issue at current scale. If Panda ever targets large-memory machines, the
question reopens — see `plans/ROADMAP.md` (M0 notes).

## SMP note

The recursive mapping is per-address-space and per-CPU (each CPU walks the
tables of whatever CR3 it has loaded). The one SMP-specific cost of this
design is that page-table modifications must also invalidate the recursive
window's TLB entries during shootdown — tracked in `plans/ROADMAP.md` M7.

## Userspace address spaces

Each process gets its own page tables (`Context`). The kernel's higher-half
PML4 entries (MMIO, heap, image, recursive slot) are shared into every
process's PML4 so syscalls and interrupts need no address-space switch;
userspace owns PML4 entries 0–255 exclusively. Layout constants live in
`panda-abi` and are documented in
[VIRTUAL_ADDRESS_SPACE.md](VIRTUAL_ADDRESS_SPACE.md).
