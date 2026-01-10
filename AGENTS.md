# Agent Guidelines for Panda

This document provides guidance for AI agents working on the Panda kernel.

## Project Overview

Panda is an experimental operating system kernel written in Rust. It runs on x86_64 with UEFI boot and includes:

- Kernel with memory management, scheduling, and syscalls
- Userspace library (libpanda) for writing applications
- VFS with initrd/tarfs support
- Resource-oriented syscall API using handles

## Documentation

- [docs/TESTING.md](docs/TESTING.md) - How to write and run kernel and userspace tests

## Key Directories

```
panda-kernel/          # Kernel code
  src/                 # Kernel source
  tests/               # Kernel integration tests
userspace/
  libpanda/            # Userspace library
  init/                # Init process
  tests/               # Userspace test programs
panda-abi/             # Shared ABI definitions (syscalls, constants)
```

## Building and Testing

```bash
# Build everything
make build

# Run kernel tests
make test

# Run userspace tests
make userspace-test

# Run in QEMU interactively
make run
```

## Syscall Architecture

The kernel uses a resource-oriented syscall design:

- Single `send(handle, operation, args...)` syscall
- Well-known handles: `HANDLE_SELF` (0), `HANDLE_ENVIRONMENT` (1)
- Operation codes grouped by resource type (File, Process, Environment)
- Type-safe handle table in kernel prevents wrong operations on handles

See `panda-abi/src/lib.rs` for operation constants.

## Code Style

- Rust 2024 edition
- `#![no_std]` for all kernel and userspace code
- Use the libpanda API modules (`environment`, `file`, `process`) not raw syscalls
- Tests should use `expected.txt` for log verification
