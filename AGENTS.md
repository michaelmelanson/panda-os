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
