# Building and Running

## Prerequisites

- Rust nightly toolchain
- QEMU with x86_64 and UEFI support
- OVMF firmware files in `firmware/`

## Build Commands

```bash
# Build kernel and userspace
make build

# Build only kernel
make panda-kernel

# Build only init
make init

# Build only shell
make shell
```

## Running

```bash
# Run in QEMU with display
make run
```

## Custom Targets

The project uses custom target specifications:

- `x86_64-panda-uefi.json` - Kernel target (UEFI, no SSE, panic=abort)
- `x86_64-panda-userspace.json` - Userspace target

These are required because the kernel runs in a freestanding environment without the standard library.

## Cargo Invocations

The Makefile wraps cargo with the correct flags:

```bash
# Kernel (uses custom UEFI target)
cargo +nightly build --package panda-kernel --target ./x86_64-panda-uefi.json

# Userspace (needs build-std for no_std)
cargo +nightly build -Z build-std=core,alloc --package init --target ./x86_64-panda-userspace.json
```
