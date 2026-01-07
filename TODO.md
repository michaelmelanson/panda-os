# Panda OS TODO

## Known Issues

- Process exit panics the kernel (`syscall.rs:140`)
- NX bit disabled (`memory/mod.rs:117`) - all user pages executable
- Single static kernel stack (4KB) - could overflow
- Bump allocator never frees memory
- 63 unsafe blocks with limited safety wrappers
- Identity mapping limits address space usage
- Global mutable state with potential deadlock risk
- ACPI handler completely unimplemented (27 todo!() macros)

## Immediate Priorities

### 1. Fix Process Exit
- Process exit currently panics the kernel
- Should clean up process and return control to scheduler
- Test process lifecycle (create, run, exit, schedule next)

### 2. Fix NX (No-Execute) Bit
- Currently disabled due to "reserved write" page fault
- All user pages are executable - security vulnerability
- Debug page table flag propagation
- Check CR4.PAE and IA32_EFER.NXE settings

### 3. Implement Context Switching
- Save/restore registers in Process struct (rsp, rip, general purpose)
- Modify syscall handler to save state before switching
- Enable timer-based preemption (timer interrupt handler is empty)
- Currently scheduler can only run one process to completion

### 4. Expand Syscalls
- Subsystem-based architecture: default capability-based Panda syscalls, optional POSIX compatibility layer
- Resource handles (capabilities) instead of file descriptors
- Message-passing with structured objects (BSON) instead of byte streams
- `sys_send(handle, object)` - send structured object to resource
- `sys_recv(handle)` - receive structured object from resource
- `sys_spawn(path)` - create new process from initrd binary
- `sys_yield()` - cooperative scheduling / early yield from I/O blocks

### 5. Basic Console I/O
- Console as capability granted to init process
- Implement write to serial output
- Simple line-buffered input from keyboard

## Medium-term Goals

### 6. Multiple Processes
- Spawn processes from initrd binaries (not fork/exec)
- Test scheduling with multiple concurrent processes
- Add `sys_getpid()`

### 7. Deadline-based Scheduler
- Optimize for latency over throughput
- Niceness for processes that yield early (I/O bound)
- Preemption based on deadlines

### 8. Proper Memory Allocator
- Replace bump allocator (never frees memory)
- Implement linked list or buddy allocator
- Add memory pressure handling / OOM

### 9. Basic VFS Abstraction
- In-memory filesystem (tmpfs)
- Capability table per process (resource handles)
- open returns capability, close releases it

### 10. Userspace Library (libpanda)
- Native Panda syscall wrappers
- printf implementation
- String/memory functions
- Userspace heap allocator

### 11. Keyboard Input
- PS/2 or Virtio input device driver
- Interrupt-driven input
- Ring buffer for key events

## Long-term Goals

### 12. Block Device & Filesystem
- Virtio block driver
- Simple FAT filesystem
- Persistent storage

### 13. Shell
- Command interpreter
- Process spawning via sys_spawn
- Built-in commands

### 14. POSIX Compatibility Subsystem
- Optional layer mapping POSIX calls to Panda syscalls
- fork/exec emulation via spawn where possible
- Compatibility for porting existing software
