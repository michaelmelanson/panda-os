# Panda OS TODO

## Immediate Priorities

### 1. Fix Process Exit
- Process exit currently panics the kernel (`syscall.rs:140`)
- Should clean up process and return control to scheduler
- Test process lifecycle (create, run, exit, schedule next)

### 2. Fix NX (No-Execute) Bit
- Currently disabled in `memory/mod.rs:117` due to "reserved write" page fault
- All user pages are executable - security vulnerability
- Debug page table flag propagation
- Check CR4.PAE and IA32_EFER.NXE settings

### 3. Implement Context Switching
- Save/restore registers in Process struct (rsp, rip, general purpose)
- Modify syscall handler to save state before switching
- Enable timer-based preemption (timer interrupt handler is empty)
- Currently scheduler can only run one process to completion

### 4. Expand Syscalls
- `sys_write(fd, buf, len)` - write to file descriptor
- `sys_read(fd, buf, len)` - read from file descriptor
- `sys_brk(addr)` - heap allocation
- `sys_yield()` - cooperative scheduling

### 5. Basic Console I/O
- Connect serial port to stdin/stdout (fd 0, 1)
- Implement write() to serial output
- Simple line-buffered input from keyboard

## Medium-term Goals

### 6. Multiple Processes
- Load multiple ELF binaries
- Test round-robin scheduling
- Add `sys_getpid()`

### 7. Proper Memory Allocator
- Replace bump allocator (never frees memory)
- Implement linked list or buddy allocator
- Add memory pressure handling / OOM

### 8. Basic VFS Abstraction
- In-memory filesystem (tmpfs)
- File descriptor table per process
- open/close/read/write on files

### 9. Userspace Library (libpanda)
- Mini libc functionality
- printf implementation
- String/memory functions
- Userspace heap allocator

### 10. Keyboard Input
- PS/2 or Virtio input device driver
- Interrupt-driven input
- Ring buffer for key events

## Long-term Goals

### 11. Block Device & Filesystem
- Virtio block driver
- Simple filesystem (FAT or custom)
- Persistent storage

### 12. Shell
- Command interpreter
- Process spawning
- Built-in commands

### 13. fork/exec/wait
- Process creation from userspace
- Copy-on-write page tables
- Parent-child relationships

### 14. Improved Scheduler
- Time slices
- Priority levels
- Fair scheduling algorithm

## Known Issues

- Single static kernel stack (4KB) - could overflow
- 63 unsafe blocks with limited safety wrappers
- Identity mapping limits address space usage
- Global mutable state with potential deadlock risk
- ACPI handler completely unimplemented (27 todo!() macros)
