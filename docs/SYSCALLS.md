# Syscall architecture

The kernel uses a resource-oriented syscall design with a single entry point.

## Design

- Single `send(handle, operation, args...)` syscall
- Well-known handles for standard resources
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

## Well-known handles

Every process has these pre-allocated handles. Handle values encode a type tag in the high 8 bits and an ID in the low 24 bits.

| Handle | Type | ID | Description |
|--------|------|-----|-------------|
| `HANDLE_STDIN` | Channel (0x10) | 0 | Standard input channel (pipeline) |
| `HANDLE_STDOUT` | Channel (0x10) | 1 | Standard output channel (pipeline) |
| `HANDLE_STDERR` | Channel (0x10) | 2 | Standard error channel (reserved) |
| `HANDLE_PROCESS` | Process (0x11) | 3 | Current process resource |
| `HANDLE_ENVIRONMENT` | Special | 4 | System environment |
| `HANDLE_MAILBOX` | Mailbox (0x20) | 5 | Default mailbox |
| `HANDLE_PARENT` | Channel (0x10) | 6 | Channel to parent process |

See `HandleType` in panda-abi for all type tags.

## Operation codes

### File operations (0x1_0000 - 0x1_FFFF)

| Operation | Code | Arguments | Returns |
|-----------|------|-----------|---------|
| `OP_FILE_READ` | 0x1_0000 | (buf_ptr, buf_len) | bytes_read |
| `OP_FILE_WRITE` | 0x1_0001 | (buf_ptr, buf_len) | bytes_written |
| `OP_FILE_SEEK` | 0x1_0002 | (offset_lo, offset_hi, whence) | new_position |
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
| `OP_ENVIRONMENT_OPEN` | 0x3_0000 | (path_ptr, path_len, mailbox, event_mask) | handle |
| `OP_ENVIRONMENT_SPAWN` | 0x3_0001 | (path_ptr, path_len, mailbox, event_mask, stdin, stdout) | process_handle |
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

### Mailbox operations (0x7_0000 - 0x7_0FFF)

| Operation | Code | Arguments | Returns |
|-----------|------|-----------|---------|
| `OP_MAILBOX_CREATE` | 0x7_0000 | () | mailbox_handle |
| `OP_MAILBOX_WAIT` | 0x7_0001 | () | (handle << 32) \| events |
| `OP_MAILBOX_POLL` | 0x7_0002 | () | (handle << 32) \| events, or 0 |

### Channel operations (0x7_1000 - 0x7_1FFF)

| Operation | Code | Arguments | Returns |
|-----------|------|-----------|---------|
| `OP_CHANNEL_CREATE` | 0x7_1000 | () | (handle_a << 32) \| handle_b |
| `OP_CHANNEL_SEND` | 0x7_1001 | (buf_ptr, buf_len, flags) | 0 or error |
| `OP_CHANNEL_RECV` | 0x7_1002 | (buf_ptr, buf_len, flags) | msg_len or error |

## Event flags

Used with mailbox operations and open/spawn event_mask:

| Flag | Value | Description |
|------|-------|-------------|
| `EVENT_CHANNEL_READABLE` | 1 << 0 | Message available to receive |
| `EVENT_CHANNEL_WRITABLE` | 1 << 1 | Space available to send |
| `EVENT_CHANNEL_CLOSED` | 1 << 2 | Peer closed their endpoint |
| `EVENT_PROCESS_EXITED` | 1 << 3 | Child process has exited |
| `EVENT_KEYBOARD_KEY` | 1 << 4 | Key event available |

## Userspace API

Use libpanda rather than raw syscalls. See [IPC.md](IPC.md) for channel and mailbox APIs.

### environment

```rust
use libpanda::environment;

environment::log("message");                              // Log to console
environment::open("/path", mailbox, events) -> Handle;    // Open file
environment::opendir("/path") -> Handle;                  // Open directory
environment::spawn("/path", mailbox, events) -> Handle;   // Spawn process
environment::mount("ext2", "/mnt");                       // Mount filesystem
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

### channel

```rust
use libpanda::ipc::{channel, Channel};

Channel::create_pair() -> (ChannelHandle, ChannelHandle);  // Create pair
channel::send(handle, &data) -> Result<()>;                // Send (blocking)
channel::try_send(handle, &data) -> Result<()>;            // Send (non-blocking)
channel::recv(handle, &mut buf) -> Result<usize>;          // Receive (blocking)
channel::try_recv(handle, &mut buf) -> Result<usize>;      // Receive (non-blocking)
```

### mailbox

```rust
use libpanda::mailbox::Mailbox;

let mailbox = Mailbox::default();               // Get default mailbox
let (handle, events) = mailbox.wait();          // Wait for event (blocking)
let result = mailbox.poll();                    // Poll for event (non-blocking)
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

/// Maximum channel message size
pub const MAX_MESSAGE_SIZE: usize = 4096;
```

## Per-process handle limit

Each process may hold at most **4096** open handles. Any syscall that creates a
handle (`OP_ENVIRONMENT_OPEN`, `OP_ENVIRONMENT_SPAWN`, `OP_ENVIRONMENT_OPENDIR`,
`OP_BUFFER_ALLOC`, `OP_CHANNEL_CREATE`, `OP_MAILBOX_CREATE`) will return `-1` if
the limit is reached. Closing handles via `OP_FILE_CLOSE` frees slots so new
handles can be created again.

## Error codes

Negative return values indicate errors. See `panda_abi::ErrorCode`:

| Code | Name | Description |
|------|------|-------------|
| -1 | NotFound | Resource not found |
| -2 | InvalidOffset | Invalid seek position |
| -3 | NotReadable | Resource is not readable |
| -4 | NotWritable | Resource is not writable |
| -5 | NotSeekable | Resource is not seekable |
| -6 | NotSupported | Operation not supported |
| -7 | PermissionDenied | Permission denied |
| -8 | IoError | I/O error |
| -10 | InvalidArgument | Invalid argument |
| -11 | WouldBlock | Operation would block (with NONBLOCK flag) |
| -12 | ChannelClosed | Channel peer has closed |
