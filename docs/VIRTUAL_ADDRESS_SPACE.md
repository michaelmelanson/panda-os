# Panda OS Virtual Address Space

## Overview

Panda uses a 48-bit virtual address space (x86-64 canonical addresses) with:
- **Identity mapping** for kernel access to physical memory (virt = phys)
- **Per-process userspace regions** in PML4 entries 20-22

## Address Space Layout

```
PML4 Index   Virtual Address Range                 Purpose
----------   ----------------------                -------
0-19         0x0_0000_0000_0000 - 0x9_ffff_ffff_ffff   Kernel (identity-mapped)
20-22        0xa_0000_0000_0000 - 0xb_ffff_ffff_ffff   Userspace (per-process)
23-255       0xc_0000_0000_0000 - 0x7fff_ffff_ffff_ffff Kernel (identity-mapped)
256-511      Non-canonical hole / high half kernel
```

## Userspace Layout (PML4 20-22)

All userspace addresses are per-process and isolated via separate page tables.

| Region | Start Address | End Address | Size | Description |
|--------|---------------|-------------|------|-------------|
| ELF Segments | `0xa00_0000_0000` | varies | varies | Code (.text), read-only data (.rodata), data (.data), BSS (.bss), GOT (.got) |
| Buffer Region | `0xaff_0000_0000` | `0xaff_ffff_ffff` | 4 GB | Zero-copy I/O buffers, allocated via `OP_BUFFER_ALLOC` |
| Stack | `0xb00_0000_0000` | `0xb00_00ff_ffff` | 16 MB | Grows downward, demand-paged |
| Heap | `0xb00_0100_0000` | `0xb00_0100_0000` + 1 TB | 1 TB max | Grows upward via `sbrk`, demand-paged |

### ELF Loading

Userspace binaries are linked with `x86_64-panda-userspace.ld`:
- Base address: `0xa00_0000_0000`
- Sections page-aligned (4 KB)
- Section order: `.text`, `.rodata`, `.data`, `.bss`, `.got`

### Stack

- Base: `STACK_BASE` = `0xb00_0000_0000`
- Max size: `STACK_MAX_SIZE` = 16 MB (`0x100_0000`)
- Initial RSP: `STACK_BASE + STACK_MAX_SIZE - 8` = `0xb00_00ff_fff8`
- Grows downward; pages allocated on demand via page fault handler

### Heap

- Base: `HEAP_BASE` = `0xb00_0100_0000`
- Max size: `HEAP_MAX_SIZE` = 1 TB (`0x100_0000_0000`)
- Managed via `sbrk` syscall (`OP_PROCESS_SBRK`)
- Pages allocated on demand via page fault handler

### Buffer Region

- Base: `BUFFER_BASE` = `0xaff_0000_0000`
- Max size: `BUFFER_MAX_SIZE` = 4 GB (`0x1_0000_0000`)
- Used for zero-copy I/O between kernel and userspace
- Allocated/freed via `OP_BUFFER_ALLOC` / `OP_BUFFER_FREE` syscalls

## Kernel Memory

The kernel uses identity mapping (virtual address = physical address) for all physical memory access. This simplifies:
- Accessing UEFI-provided memory maps
- DMA buffer management
- MMIO register access

### MMIO Regions

PCI devices with 64-bit BARs may have MMIO regions at high physical addresses (> 4 GB). The kernel's `map_mmio()` function creates identity mappings for these regions on demand.

## Constants (panda-abi)

```rust
pub const BUFFER_BASE: usize = 0xaff_0000_0000;
pub const BUFFER_MAX_SIZE: usize = 0x1_0000_0000;      // 4 GB

pub const STACK_BASE: usize = 0xb00_0000_0000;
pub const STACK_MAX_SIZE: usize = 0x100_0000;          // 16 MB

pub const HEAP_BASE: usize = 0xb00_0100_0000;
pub const HEAP_MAX_SIZE: usize = 0x100_0000_0000;      // 1 TB
```

## Page Table Structure

Userspace regions use PML4 entries 20-22:
- Entry 20: `0xa00_0000_0000` - ELF segments
- Entry 21: `0xa80_0000_0000` - (unused, part of buffer region)
- Entry 22: `0xb00_0000_0000` - Stack and heap

On context switch, only PML4 entries 20-22 are swapped; kernel mappings (all other entries) remain shared across all processes.
