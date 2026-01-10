# Panda OS TODO

## Known Issues

- Single static kernel stack (4KB) - could overflow
- 63 unsafe blocks with limited safety wrappers
- Identity mapping limits address space usage
- Global mutable state with potential deadlock risk
- ACPI handler completely unimplemented (27 todo!() macros)
- **proc-macro2 >= 1.0.104 causes test failures**: The `log!` macros generate incorrect code when used in x86-interrupt handlers with proc-macro2 1.0.104+. This causes register corruption during timer interrupts, leading to userspace computation errors. Cargo.lock pins proc-macro2 to 1.0.103 as a workaround. See upstream issue investigation needed.

## Immediate Priorities

### Expand Syscalls
- Subsystem-based architecture: default capability-based Panda syscalls, optional POSIX compatibility layer
- Resource handles (capabilities) instead of file descriptors
- Message-passing with structured objects (BSON) instead of byte streams
- `sys_send(handle, object)` - send structured object to resource
- `sys_recv(handle)` - receive structured object from resource

### Basic Console I/O
- Console as capability granted to init process
- Implement write to serial output
- Simple line-buffered input from keyboard

## Medium-term Goals

### Deadline-based Scheduler
- Optimize for latency over throughput
- Niceness for processes that yield early (I/O bound)
- Preemption based on deadlines

### Memory Pressure Handling
- Add OOM (out-of-memory) handling
- Consider adding memory pressure notifications

### Basic VFS Abstraction
- In-memory filesystem (tmpfs)
- Capability table per process (resource handles)
- open returns capability, close releases it

### Userspace Library (libpanda)
- printf implementation
- String/memory functions
- Userspace heap allocator

### Keyboard Input
- PS/2 or Virtio input device driver
- Interrupt-driven input
- Ring buffer for key events

## Long-term Goals

### Block Device & Filesystem
- Virtio block driver
- Simple FAT filesystem
- Persistent storage

### Shell
- Command interpreter
- Process spawning via sys_spawn
- Built-in commands

### POSIX Compatibility Subsystem
- Optional layer mapping POSIX calls to Panda syscalls
- fork/exec emulation via spawn where possible
- Compatibility for porting existing software
