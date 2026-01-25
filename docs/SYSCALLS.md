# Syscall architecture

The kernel uses a resource-oriented syscall design with a single entry point.

## Design

- Single `send(handle, operation, args...)` syscall
- Well-known handles: `HANDLE_SELF` (0), `HANDLE_ENVIRONMENT` (1)
- Operation codes grouped by resource type
- Type-safe handle table in kernel prevents wrong operations on handles

See `panda-abi/src/lib.rs` for operation constants and shared types.

## Syscall ABI

The syscall uses the standard x86_64 syscall convention:

- `rax`: syscall code (always `SYSCALL_SEND = 0x30`)
- `rdi`: handle
- `rsi`: operation code
- `rdx`, `r10`, `r8`, `r9`: operation arguments (up to 4)
- Return value in `rax`

The `syscall` instruction clobbers `rcx` (saves RIP) and `r11` (saves RFLAGS).

## Callee-saved register preservation

The kernel preserves callee-saved registers (rbx, rbp, r12-r15) across syscalls:

- **Normal syscalls**: Registers are saved on the kernel stack in `syscall_entry` and restored before `sysretq`
- **Blocking syscalls**: When a syscall blocks, the kernel saves all registers in `SavedState` so they can be restored when the process resumes
- **Yield**: Returns immediately with rax=0

## Blocking and wakers

When a syscall cannot complete immediately (e.g., `OP_FILE_READ` on a keyboard with no input):

1. The syscall handler returns `WouldBlock` with a waker
2. The kernel saves the process state including:
   - Return IP (pointing to instruction after syscall for re-poll)
   - User stack pointer
   - All syscall argument registers
   - All callee-saved registers
3. The process is marked as `Blocked` and the waker is registered
4. When data becomes available, the device calls `waker.wake()`
5. The process resumes and the syscall is re-polled

## Operation codes

### File operations (0x1_0000 - 0x1_FFFF)

| Operation | Code | Arguments | Returns |
|-----------|------|-----------|---------|
| `OP_FILE_READ` | 0x1_0000 | (buf_ptr, buf_len) | bytes_read |
| `OP_FILE_WRITE` | 0x1_0001 | (buf_ptr, buf_len) | bytes_written |
| `OP_FILE_SEEK` | 0x1_0002 | (offset, whence) | new_position |
| `OP_FILE_STAT` | 0x1_0003 | (stat_ptr) | 0 or error |
| `OP_FILE_CLOSE` | 0x1_0004 | () | 0 or error |
| `OP_FILE_READDIR` | 0x1_0005 | (entry_ptr) | 1=entry, 0=end, <0=error |

### Process operations (0x2_0000 - 0x2_FFFF)

| Operation | Code | Arguments | Returns |
|-----------|------|-----------|---------|
| `OP_PROCESS_YIELD` | 0x2_0000 | () | 0 |
| `OP_PROCESS_EXIT` | 0x2_0001 | (code) | ! (never returns) |
| `OP_PROCESS_GET_PID` | 0x2_0002 | () | pid |
| `OP_PROCESS_WAIT` | 0x2_0003 | () | exit_code or error |
| `OP_PROCESS_SIGNAL` | 0x2_0004 | (signal) | 0 or error |
| `OP_PROCESS_BRK` | 0x2_0005 | (new_brk) | current_brk |

### Environment operations (0x3_0000 - 0x3_FFFF)

| Operation | Code | Arguments | Returns |
|-----------|------|-----------|---------|
| `OP_ENVIRONMENT_OPEN` | 0x3_0000 | (path_ptr, path_len, flags) | handle |
| `OP_ENVIRONMENT_SPAWN` | 0x3_0001 | (path_ptr, path_len) | process_handle |
| `OP_ENVIRONMENT_LOG` | 0x3_0002 | (msg_ptr, msg_len) | 0 |
| `OP_ENVIRONMENT_TIME` | 0x3_0003 | () | timestamp |
| `OP_ENVIRONMENT_OPENDIR` | 0x3_0004 | (path_ptr, path_len) | dir_handle |
| `OP_ENVIRONMENT_MOUNT` | 0x3_0005 | (fstype_ptr, fstype_len, mount_ptr, mount_len) | 0 or error |

### Buffer operations (0x4_0000 - 0x4_FFFF)

| Operation | Code | Arguments | Returns |
|-----------|------|-----------|---------|
| `OP_BUFFER_ALLOC` | 0x4_0000 | (size, info_ptr) | buffer_handle |
| `OP_BUFFER_RESIZE` | 0x4_0002 | (new_size, info_ptr) | 0 or error |
| `OP_BUFFER_FREE` | 0x4_0003 | () | 0 or error |

### Buffer-based file operations (0x5_0000 - 0x5_FFFF)

| Operation | Code | Arguments | Returns |
|-----------|------|-----------|---------|
| `OP_FILE_READ_BUFFER` | 0x5_0000 | (buffer_handle) | bytes_read |
| `OP_FILE_WRITE_BUFFER` | 0x5_0001 | (buffer_handle, len) | bytes_written |

### Surface operations (0x6_0000 - 0x6_FFFF)

| Operation | Code | Arguments | Returns |
|-----------|------|-----------|---------|
| `OP_SURFACE_INFO` | 0x6_0000 | (info_ptr) | 0 or error |
| `OP_SURFACE_BLIT` | 0x6_0001 | (params_ptr) | 0 or error |
| `OP_SURFACE_FILL` | 0x6_0002 | (params_ptr) | 0 or error |
| `OP_SURFACE_FLUSH` | 0x6_0003 | (rect_ptr) | 0 or error |
| `OP_SURFACE_UPDATE_PARAMS` | 0x6_0004 | (params_ptr) | 0 or error |

## Userspace API

Use libpanda rather than raw syscalls. The API is organised by module:

### environment

```rust
use libpanda::environment;

environment::log("message");                    // Log to console
environment::open("/path", flags) -> Handle;    // Open file
environment::opendir("/path") -> Handle;        // Open directory
environment::spawn("/path") -> Handle;          // Spawn process
environment::time() -> isize;                   // Get system time
environment::mount("ext2", "/mnt");             // Mount filesystem
```

### file

```rust
use libpanda::file;

file::read(handle, &mut buf) -> isize;          // Read bytes
file::write(handle, &buf) -> isize;             // Write bytes
file::seek(handle, offset, whence) -> isize;    // Seek position
file::stat(handle, &mut stat) -> isize;         // Get file stats
file::readdir(handle, &mut entry) -> isize;     // Read directory entry
file::close(handle) -> isize;                   // Close handle
```

### process

```rust
use libpanda::process;

process::yield_now();                           // Yield CPU
process::exit(code) -> !;                       // Exit process
process::getpid() -> u64;                       // Get process ID
process::wait(child_handle) -> i32;             // Wait for child
process::signal(handle, sig) -> isize;          // Send signal
```

### buffer

```rust
use libpanda::buffer::Buffer;

let buf = Buffer::alloc(size)?;                 // Allocate shared buffer
buf.as_slice();                                 // Get buffer contents
buf.as_mut_slice();                             // Get mutable contents
buf.resize(new_size)?;                          // Resize buffer
buf.read_from(file_handle)?;                    // Read file into buffer
buf.write_to(file_handle, len)?;                // Write buffer to file
// Buffer is freed on drop
```

## Shared types

Defined in `panda-abi`:

```rust
/// File metadata
pub struct FileStat {
    pub size: u64,
    pub is_dir: bool,
}

/// Directory entry (for readdir)
pub struct DirEntry {
    pub name_len: u8,
    pub is_dir: bool,
    pub name: [u8; 255],
}

/// Buffer allocation info
pub struct BufferAllocInfo {
    pub addr: usize,
    pub size: usize,
}

/// Seek whence values
pub const SEEK_SET: u32 = 0;
pub const SEEK_CUR: u32 = 1;
pub const SEEK_END: u32 = 2;
```

## Error codes

Negative return values indicate errors. See `panda_abi::ErrorCode`:

| Code | Name | Description |
|------|------|-------------|
| 0 | Ok | Success |
| 1 | NotFound | Resource not found |
| 2 | InvalidOffset | Invalid seek position |
| 3 | NotReadable | Resource is not readable |
| 4 | NotWritable | Resource is not writable |
| 5 | NotSeekable | Resource is not seekable |
| 6 | NotSupported | Operation not supported |
| 7 | PermissionDenied | Permission denied |
| 8 | IoError | I/O error |
| 10 | InvalidArgument | Invalid argument |
