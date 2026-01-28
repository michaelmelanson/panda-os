# Panda OS Virtual Address Space

## Overview

Panda uses a higher-half kernel design with:
- **Higher-half kernel** in the upper 128 TB of virtual address space
- **Userspace regions** in the lower 128 TB (canonical low half)
- **Demand paging** for stack and heap

## Address Space Layout

```
Virtual Address Range                    Purpose
---------------------                    -------
0x0000_0000_0000_0000 - 0x0000_7fff_ffff_ffff   Userspace (lower canonical half)
0x0000_8000_0000_0000 - 0xffff_7fff_ffff_ffff   Non-canonical hole (invalid)
0xffff_8000_0000_0000 - 0xffff_ffff_ffff_ffff   Kernel (higher canonical half)
```

## Userspace Layout

All userspace addresses are per-process and isolated via separate page tables.

| Region | Start Address | Size | Description |
|--------|---------------|------|-------------|
| ELF Segments | `0x0000_0000_0040_0000` | varies | Code, data, BSS |
| Heap | `0x0000_0001_0000_0000` | 1 TB max | Grows upward via `brk` |
| Buffer Region | `0x0000_0100_0000_0000` | 4 GB | Zero-copy I/O buffers |
| Stack | `0x0000_7fff_fef0_0000` | 16 MB | Grows downward |

### ELF Loading

Userspace binaries are loaded at a low address with sections page-aligned (4 KB).

### Stack

- Base: `STACK_BASE` = `0x0000_7fff_fef0_0000`
- Max size: `STACK_MAX_SIZE` = 16 MB (`0x100_0000`)
- Initial RSP: `STACK_BASE + STACK_MAX_SIZE` (grows downward)
- Pages allocated on demand via page fault handler

### Heap

- Base: `HEAP_BASE` = `0x0000_0001_0000_0000`
- Max size: `HEAP_MAX_SIZE` = 1 TB (`0x100_0000_0000`)
- Managed via `brk` syscall (`OP_PROCESS_BRK`)
- Pages allocated on demand via page fault handler

### Buffer Region

- Base: `BUFFER_BASE` = `0x0000_0100_0000_0000`
- Max size: `BUFFER_MAX_SIZE` = 4 GB (`0x1_0000_0000`)
- Used for zero-copy I/O between kernel and userspace
- Allocated/freed via `OP_BUFFER_ALLOC` / `OP_BUFFER_FREE` syscalls

## Kernel Memory

The kernel runs in the higher half with:
- Kernel code and data mapped at high addresses
- Physical memory accessible via offset mapping
- MMIO regions mapped on demand

See [HIGHER_HALF_KERNEL.md](HIGHER_HALF_KERNEL.md) for details on the kernel memory layout.

## Constants (panda-abi)

```rust
pub const BUFFER_BASE: usize = 0x0000_0100_0000_0000;
pub const BUFFER_MAX_SIZE: usize = 0x1_0000_0000;      // 4 GB

pub const STACK_BASE: usize = 0x0000_7fff_fef0_0000;
pub const STACK_MAX_SIZE: usize = 0x100_0000;          // 16 MB

pub const HEAP_BASE: usize = 0x0000_0001_0000_0000;
pub const HEAP_MAX_SIZE: usize = 0x100_0000_0000;      // 1 TB
```

## Page Table Structure

On context switch, userspace page table entries are swapped while kernel mappings remain shared across all processes.
