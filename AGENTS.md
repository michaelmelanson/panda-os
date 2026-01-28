# Agent Guidelines for Panda

This file is a table of contents into the docs/ directory. Keep detailed documentation there, not here. Keep them up to date as the system evolves.

## Style

- Use Canadian English in chat and code (e.g., colour, behaviour, centre)
- Use "Sentence case" rather than "Title Case" for headers.

## Documentation

- [docs/BUILDING.md](docs/BUILDING.md) - Build commands, custom targets, cargo invocations
- [docs/TESTING.md](docs/TESTING.md) - Writing and running kernel and userspace tests
- [docs/SYSCALLS.md](docs/SYSCALLS.md) - Syscall ABI, register conventions, blocking behaviour
- [docs/KERNEL_INTERNALS.md](docs/KERNEL_INTERNALS.md) - Syscall entry/exit, process states, wakers
- [docs/ASYNC_VFS_EXT2.md](docs/ASYNC_VFS_EXT2.md) - Async VFS layer, BlockDevice trait, ext2 filesystem
- [docs/HIGHER_HALF_KERNEL.md](docs/HIGHER_HALF_KERNEL.md) - Higher-half kernel memory layout and relocation
- [docs/VIRTUAL_ADDRESS_SPACE.md](docs/VIRTUAL_ADDRESS_SPACE.md) - Virtual address space layout for kernel and userspace
- [docs/DEVICE_PATHS.md](docs/DEVICE_PATHS.md) - Unified device path scheme with human-friendly names
- [docs/IPC.md](docs/IPC.md) - Channels, mailboxes, and process communication
- [docs/PIPELINES.md](docs/PIPELINES.md) - Structured Value-based pipelines

## Quick Reference

```bash
make build          # Build kernel and userspace
make test           # Run all tests
make run            # Run in QEMU
```

## Directory Structure

```
panda-kernel/src/   # Kernel source
panda-kernel/tests/ # Kernel integration tests
userspace/libpanda/ # Userspace library
userspace/tests/    # Userspace test programs
panda-abi/          # Shared syscall definitions
docs/               # Detailed documentation
```
