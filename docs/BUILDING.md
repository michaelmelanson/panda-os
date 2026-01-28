# Building and running

## Prerequisites

- Rust nightly toolchain
- QEMU with x86_64 and UEFI support
- OVMF firmware files in `firmware/`

## Build commands

```bash
# Build kernel and userspace
make build

# Build only kernel
make panda-kernel

# Build only specific userspace programs
make init
make terminal
make hello
make ls
make cat
```

## Running

```bash
# Run in QEMU with display
make run
```

## Testing

```bash
# Run all tests
make test

# Run only kernel tests
make kernel-test

# Run only userspace tests
make userspace-test

# Run a specific test
make kernel-test TEST=heap
make userspace-test TEST=channel_test
```

See [TESTING.md](TESTING.md) for details on writing tests.

## Custom targets

The project uses custom target specifications:

- `x86_64-panda-uefi.json` - Kernel target (UEFI, no SSE, panic=abort)
- `x86_64-panda-userspace.json` - Userspace target

These are required because both kernel and userspace run in freestanding environments without the standard library.

## Cargo invocations

The Makefile wraps cargo with the correct flags:

```bash
# Kernel
cargo +nightly build \
    -Z build-std=core,alloc \
    -Z build-std-features=compiler-builtins-mem \
    --package panda-kernel \
    --target ./x86_64-panda-uefi.json

# Userspace
cargo +nightly build \
    -Z build-std=core,alloc \
    -Z build-std-features=compiler-builtins-mem \
    --package init \
    --target ./x86_64-panda-userspace.json
```

## Ext2 test image

Some tests require an ext2 filesystem image:

```bash
# Create/update the ext2 test image
make ext2-image

# Clean the ext2 image (forces rebuild)
make clean-ext2
```
