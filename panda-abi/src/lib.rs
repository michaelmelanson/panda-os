//! Shared ABI definitions between kernel and userspace.
//!
//! This crate contains syscall numbers, constants, and shared types
//! that both the kernel and userspace need to agree on.

#![no_std]

pub mod encoding;
pub mod terminal;

// =============================================================================
// Syscall numbers
// =============================================================================

/// The unified send syscall - all operations go through this
pub const SYSCALL_SEND: usize = 0x30;

// =============================================================================
// Well-known handles
// =============================================================================

/// Well-known handle IDs that are pre-allocated for every process.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WellKnownHandle {
    /// Handle to the current process (Process resource)
    ProcessSelf = 0,
    /// Handle to the system environment (Environment resource)
    Environment = 1,
    /// Handle to the process's default mailbox (Mailbox resource)
    Mailbox = 2,
    /// Handle to the channel connected to the parent process (ChannelEndpoint resource)
    /// Only valid if this process was spawned by another process.
    Parent = 3,
}

impl WellKnownHandle {
    /// Convert to raw handle ID.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

// Legacy constants for backwards compatibility
/// Handle to the current process (Process resource)
pub const HANDLE_SELF: u32 = WellKnownHandle::ProcessSelf as u32;
/// Handle to the system environment (Environment resource)
pub const HANDLE_ENVIRONMENT: u32 = WellKnownHandle::Environment as u32;
/// Handle to the process's default mailbox (Mailbox resource)
pub const HANDLE_MAILBOX: u32 = WellKnownHandle::Mailbox as u32;
/// Handle to the channel connected to the parent process (ChannelEndpoint resource)
pub const HANDLE_PARENT: u32 = WellKnownHandle::Parent as u32;

// =============================================================================
// Operation codes
// =============================================================================

/// Syscall operation codes.
///
/// Operations are grouped by category with distinct address ranges:
/// - File operations: 0x1_0000 - 0x1_FFFF
/// - Process operations: 0x2_0000 - 0x2_FFFF
/// - Environment operations: 0x3_0000 - 0x3_FFFF
/// - Buffer operations: 0x4_0000 - 0x4_FFFF
/// - Buffer-based file operations: 0x5_0000 - 0x5_FFFF
/// - Surface operations: 0x6_0000 - 0x6_FFFF
/// - Mailbox operations: 0x7_0000 - 0x7_0FFF
/// - Channel operations: 0x7_1000 - 0x7_1FFF
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    // File operations (0x1_0000 - 0x1_FFFF)
    /// Read from file: (buf_ptr, buf_len) -> bytes_read
    FileRead = 0x1_0000,
    /// Write to file: (buf_ptr, buf_len) -> bytes_written
    FileWrite = 0x1_0001,
    /// Seek in file: (offset_lo, offset_hi, whence) -> new_position
    FileSeek = 0x1_0002,
    /// Get file stats: (stat_ptr) -> 0 or error
    FileStat = 0x1_0003,
    /// Close file: () -> 0 or error
    FileClose = 0x1_0004,
    /// Read directory entry: (entry_ptr) -> 1 if entry read, 0 if end of directory, negative on error
    FileReaddir = 0x1_0005,

    // Process operations (0x2_0000 - 0x2_FFFF)
    /// Yield execution: () -> 0
    ProcessYield = 0x2_0000,
    /// Exit process: (code) -> !
    ProcessExit = 0x2_0001,
    /// Get process ID: () -> pid
    ProcessGetPid = 0x2_0002,
    /// Wait for child: () -> exit_code or error
    ProcessWait = 0x2_0003,
    /// Signal process: (signal) -> 0 or error
    ProcessSignal = 0x2_0004,
    /// Set program break: (new_brk) -> current_brk or error
    ProcessBrk = 0x2_0005,

    // Environment operations (0x3_0000 - 0x3_FFFF)
    /// Open file: (path_ptr, path_len, flags) -> handle
    EnvironmentOpen = 0x3_0000,
    /// Spawn process: (path_ptr, path_len) -> process_handle
    EnvironmentSpawn = 0x3_0001,
    /// Log message: (msg_ptr, msg_len) -> 0
    EnvironmentLog = 0x3_0002,
    /// Get time: () -> timestamp
    EnvironmentTime = 0x3_0003,
    /// Open directory: (path_ptr, path_len) -> dir_handle or error
    EnvironmentOpendir = 0x3_0004,
    /// Mount filesystem: (fstype_ptr, fstype_len, mountpoint_ptr, mountpoint_len) -> 0 or error
    EnvironmentMount = 0x3_0005,

    // Buffer operations (0x4_0000 - 0x4_FFFF)
    /// Allocate a shared buffer: (size, info_ptr) -> buffer_handle or error
    BufferAlloc = 0x4_0000,
    /// Resize a buffer: (buffer_handle, new_size, info_ptr) -> 0 or error
    BufferResize = 0x4_0002,
    /// Free a buffer: (buffer_handle) -> 0 or error
    BufferFree = 0x4_0003,

    // Buffer-based file operations (0x5_0000 - 0x5_FFFF)
    /// Read from file into buffer: (file_handle, buffer_handle) -> bytes_read
    FileReadBuffer = 0x5_0000,
    /// Write from buffer to file: (file_handle, buffer_handle, len) -> bytes_written
    FileWriteBuffer = 0x5_0001,

    // Surface operations (0x6_0000 - 0x6_FFFF)
    /// Get surface info: (info_ptr) -> 0 or error
    SurfaceInfo = 0x6_0000,
    /// Blit pixels to surface: (params_ptr) -> 0 or error
    SurfaceBlit = 0x6_0001,
    /// Fill rectangle with solid color: (params_ptr) -> 0 or error
    SurfaceFill = 0x6_0002,
    /// Flush surface updates: (rect_ptr) -> 0 or error
    SurfaceFlush = 0x6_0003,
    /// Update window parameters: (params_ptr) -> 0 or error
    SurfaceUpdateParams = 0x6_0004,

    // Mailbox operations (0x7_0000 - 0x7_0FFF)
    /// Create a new mailbox: () -> mailbox_handle
    MailboxCreate = 0x7_0000,
    /// Wait for an event on any attached handle (blocking): (mailbox) -> (handle, events)
    MailboxWait = 0x7_0001,
    /// Poll for an event on any attached handle (non-blocking): (mailbox) -> (handle, events) or (0, 0)
    MailboxPoll = 0x7_0002,

    // Channel operations (0x7_1000 - 0x7_1FFF)
    /// Send a message on a channel: (handle, buf_ptr, buf_len, flags) -> 0 or error
    ChannelSend = 0x7_1000,
    /// Receive a message from a channel: (handle, buf_ptr, buf_len, flags) -> msg_len or error
    ChannelRecv = 0x7_1001,
}

impl Operation {
    /// Convert to raw operation code.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Try to convert from raw operation code.
    pub const fn from_u32(value: u32) -> Option<Self> {
        match value {
            0x1_0000 => Some(Self::FileRead),
            0x1_0001 => Some(Self::FileWrite),
            0x1_0002 => Some(Self::FileSeek),
            0x1_0003 => Some(Self::FileStat),
            0x1_0004 => Some(Self::FileClose),
            0x1_0005 => Some(Self::FileReaddir),
            0x2_0000 => Some(Self::ProcessYield),
            0x2_0001 => Some(Self::ProcessExit),
            0x2_0002 => Some(Self::ProcessGetPid),
            0x2_0003 => Some(Self::ProcessWait),
            0x2_0004 => Some(Self::ProcessSignal),
            0x2_0005 => Some(Self::ProcessBrk),
            0x3_0000 => Some(Self::EnvironmentOpen),
            0x3_0001 => Some(Self::EnvironmentSpawn),
            0x3_0002 => Some(Self::EnvironmentLog),
            0x3_0003 => Some(Self::EnvironmentTime),
            0x3_0004 => Some(Self::EnvironmentOpendir),
            0x3_0005 => Some(Self::EnvironmentMount),
            0x4_0000 => Some(Self::BufferAlloc),
            0x4_0002 => Some(Self::BufferResize),
            0x4_0003 => Some(Self::BufferFree),
            0x5_0000 => Some(Self::FileReadBuffer),
            0x5_0001 => Some(Self::FileWriteBuffer),
            0x6_0000 => Some(Self::SurfaceInfo),
            0x6_0001 => Some(Self::SurfaceBlit),
            0x6_0002 => Some(Self::SurfaceFill),
            0x6_0003 => Some(Self::SurfaceFlush),
            0x6_0004 => Some(Self::SurfaceUpdateParams),
            0x7_0000 => Some(Self::MailboxCreate),
            0x7_0001 => Some(Self::MailboxWait),
            0x7_0002 => Some(Self::MailboxPoll),
            0x7_1000 => Some(Self::ChannelSend),
            0x7_1001 => Some(Self::ChannelRecv),
            _ => None,
        }
    }
}

// Legacy constants for backwards compatibility
// File operations (0x1_0000 - 0x1_FFFF)
/// Read from file: (buf_ptr, buf_len) -> bytes_read
pub const OP_FILE_READ: u32 = Operation::FileRead as u32;
/// Write to file: (buf_ptr, buf_len) -> bytes_written
pub const OP_FILE_WRITE: u32 = Operation::FileWrite as u32;
/// Seek in file: (offset_lo, offset_hi, whence) -> new_position
pub const OP_FILE_SEEK: u32 = Operation::FileSeek as u32;
/// Get file stats: (stat_ptr) -> 0 or error
pub const OP_FILE_STAT: u32 = Operation::FileStat as u32;
/// Close file: () -> 0 or error
pub const OP_FILE_CLOSE: u32 = Operation::FileClose as u32;
/// Read directory entry: (entry_ptr) -> 1 if entry read, 0 if end of directory, negative on error
pub const OP_FILE_READDIR: u32 = Operation::FileReaddir as u32;

// Process operations (0x2_0000 - 0x2_FFFF)
/// Yield execution: () -> 0
pub const OP_PROCESS_YIELD: u32 = Operation::ProcessYield as u32;
/// Exit process: (code) -> !
pub const OP_PROCESS_EXIT: u32 = Operation::ProcessExit as u32;
/// Get process ID: () -> pid
pub const OP_PROCESS_GET_PID: u32 = Operation::ProcessGetPid as u32;
/// Wait for child: () -> exit_code or error
pub const OP_PROCESS_WAIT: u32 = Operation::ProcessWait as u32;
/// Signal process: (signal) -> 0 or error
pub const OP_PROCESS_SIGNAL: u32 = Operation::ProcessSignal as u32;
/// Set program break: (new_brk) -> current_brk or error
/// If new_brk is 0, returns current break without changing it.
/// Pages are allocated on demand via page faults.
pub const OP_PROCESS_BRK: u32 = Operation::ProcessBrk as u32;

// Userspace buffer region constants
/// Base address of the userspace buffer region.
/// Buffers are shared memory regions for zero-copy I/O.
/// Located in lower canonical half for higher-half kernel layout.
pub const BUFFER_BASE: usize = 0x0000_0100_0000_0000;
/// Maximum size of the userspace buffer region (4 GB virtual address space).
pub const BUFFER_MAX_SIZE: usize = 0x1_0000_0000;

// Userspace stack region constants
/// Base address of the userspace stack region.
/// Stack grows downward from STACK_BASE + STACK_MAX_SIZE.
/// Located near top of lower canonical half (0x7fff_ffff_ffff is max).
/// Stack top must be below 0x8000_0000_0000 (start of non-canonical hole).
pub const STACK_BASE: usize = 0x0000_7fff_fef0_0000;
/// Maximum size of the userspace stack (16 MB virtual address space).
/// Actual physical memory is allocated on demand via page faults.
pub const STACK_MAX_SIZE: usize = 0x100_0000;

// Userspace heap region constants
/// Base address of the userspace heap region.
/// Located in lower canonical half, grows upward.
pub const HEAP_BASE: usize = 0x0000_0001_0000_0000;
/// Maximum size of the userspace heap (1 TB virtual address space)
/// Actual physical memory is allocated on demand via page faults.
pub const HEAP_MAX_SIZE: usize = 0x100_0000_0000;

// Environment operations (0x3_0000 - 0x3_FFFF)
/// Open file: (path_ptr, path_len, flags) -> handle
pub const OP_ENVIRONMENT_OPEN: u32 = Operation::EnvironmentOpen as u32;
/// Spawn process: (path_ptr, path_len) -> process_handle
pub const OP_ENVIRONMENT_SPAWN: u32 = Operation::EnvironmentSpawn as u32;
/// Log message: (msg_ptr, msg_len) -> 0
pub const OP_ENVIRONMENT_LOG: u32 = Operation::EnvironmentLog as u32;
/// Get time: () -> timestamp
pub const OP_ENVIRONMENT_TIME: u32 = Operation::EnvironmentTime as u32;
/// Open directory: (path_ptr, path_len) -> dir_handle or error
pub const OP_ENVIRONMENT_OPENDIR: u32 = Operation::EnvironmentOpendir as u32;
/// Mount filesystem: (fstype_ptr, fstype_len, mountpoint_ptr, mountpoint_len) -> 0 or error
/// fstype: "ext2" to mount ext2 on first block device
/// mountpoint: e.g., "/mnt"
pub const OP_ENVIRONMENT_MOUNT: u32 = Operation::EnvironmentMount as u32;

// Buffer operations (0x4_0000 - 0x4_FFFF)
/// Allocate a shared buffer: (size, info_ptr) -> buffer_handle or error
/// If info_ptr is non-zero, writes BufferAllocInfo (addr, size) to that address.
pub const OP_BUFFER_ALLOC: u32 = Operation::BufferAlloc as u32;
/// Resize a buffer: (buffer_handle, new_size, info_ptr) -> 0 or error
/// If info_ptr is non-zero, writes BufferAllocInfo (addr, size) to that address.
pub const OP_BUFFER_RESIZE: u32 = Operation::BufferResize as u32;
/// Free a buffer: (buffer_handle) -> 0 or error
pub const OP_BUFFER_FREE: u32 = Operation::BufferFree as u32;

/// Buffer allocation info returned via pointer in BUFFER_ALLOC.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BufferAllocInfo {
    pub addr: usize,
    pub size: usize,
}

// Buffer-based file operations (0x5_0000 - 0x5_FFFF)
/// Read from file into buffer: (file_handle, buffer_handle) -> bytes_read
pub const OP_FILE_READ_BUFFER: u32 = Operation::FileReadBuffer as u32;
/// Write from buffer to file: (file_handle, buffer_handle, len) -> bytes_written
pub const OP_FILE_WRITE_BUFFER: u32 = Operation::FileWriteBuffer as u32;

// Surface operations (0x6_0000 - 0x6_FFFF)
/// Get surface info: (info_ptr) -> 0 or error
pub const OP_SURFACE_INFO: u32 = Operation::SurfaceInfo as u32;
/// Blit pixels to surface: (params_ptr) -> 0 or error
pub const OP_SURFACE_BLIT: u32 = Operation::SurfaceBlit as u32;
/// Fill rectangle with solid color: (params_ptr) -> 0 or error
pub const OP_SURFACE_FILL: u32 = Operation::SurfaceFill as u32;
/// Flush surface updates: (rect_ptr) -> 0 or error (rect_ptr can be 0 for full flush)
pub const OP_SURFACE_FLUSH: u32 = Operation::SurfaceFlush as u32;
/// Update window parameters: (params_ptr) -> 0 or error
pub const OP_SURFACE_UPDATE_PARAMS: u32 = Operation::SurfaceUpdateParams as u32;

// Mailbox operations (0x7_0000 - 0x7_0FFF)
/// Create a new mailbox: () -> mailbox_handle
pub const OP_MAILBOX_CREATE: u32 = Operation::MailboxCreate as u32;
/// Wait for an event on any attached handle (blocking): (mailbox) -> (handle, events)
pub const OP_MAILBOX_WAIT: u32 = Operation::MailboxWait as u32;
/// Poll for an event on any attached handle (non-blocking): (mailbox) -> (handle, events) or (0, 0)
pub const OP_MAILBOX_POLL: u32 = Operation::MailboxPoll as u32;

// Channel operations (0x7_1000 - 0x7_1FFF)
/// Send a message on a channel: (handle, buf_ptr, buf_len, flags) -> 0 or error
pub const OP_CHANNEL_SEND: u32 = Operation::ChannelSend as u32;
/// Receive a message from a channel: (handle, buf_ptr, buf_len, flags) -> msg_len or error
pub const OP_CHANNEL_RECV: u32 = Operation::ChannelRecv as u32;

// =============================================================================
// Constants
// =============================================================================

/// Seek origin for file positioning.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekFrom {
    /// Seek from the beginning of the file.
    Start = 0,
    /// Seek from the current position.
    Current = 1,
    /// Seek from the end of the file.
    End = 2,
}

impl SeekFrom {
    /// Convert to raw whence value.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Try to convert from raw whence value.
    pub const fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Start),
            1 => Some(Self::Current),
            2 => Some(Self::End),
            _ => None,
        }
    }
}

// Legacy seek whence constants
pub const SEEK_SET: u32 = SeekFrom::Start as u32;
pub const SEEK_CUR: u32 = SeekFrom::Current as u32;
pub const SEEK_END: u32 = SeekFrom::End as u32;

/// File operation flags.
///
/// These flags can be combined with bitwise OR.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFlags(pub u32);

impl FileFlags {
    /// No flags set.
    pub const NONE: Self = Self(0);
    /// Non-blocking operation: return immediately if operation would block.
    pub const NONBLOCK: Self = Self(1 << 0);

    /// Check if nonblock flag is set.
    #[inline]
    pub const fn is_nonblock(self) -> bool {
        self.0 & Self::NONBLOCK.0 != 0
    }

    /// Combine flags with bitwise OR.
    #[inline]
    pub const fn or(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

// Legacy file flags
/// Non-blocking read: return immediately if no data available.
pub const FILE_NONBLOCK: u32 = FileFlags::NONBLOCK.0;

// Channel constants
/// Maximum size of a single channel message in bytes.
/// Larger data should use shared memory / buffer handles.
pub const MAX_MESSAGE_SIZE: usize = 1024;

/// Default queue depth (number of messages per direction).
pub const DEFAULT_QUEUE_CAPACITY: usize = 16;

/// Channel operation flags.
///
/// These flags can be combined with bitwise OR.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelFlags(pub u32);

impl ChannelFlags {
    /// No flags set.
    pub const NONE: Self = Self(0);
    /// Non-blocking operation: return error immediately instead of blocking.
    pub const NONBLOCK: Self = Self(1 << 0);

    /// Check if nonblock flag is set.
    #[inline]
    pub const fn is_nonblock(self) -> bool {
        self.0 & Self::NONBLOCK.0 != 0
    }

    /// Combine flags with bitwise OR.
    #[inline]
    pub const fn or(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

// Legacy channel flags
/// Don't block if operation would wait; return error immediately instead.
pub const CHANNEL_NONBLOCK: u32 = ChannelFlags::NONBLOCK.0;

// =============================================================================
// Resource-specific event flags
// =============================================================================

/// Event flags returned by mailbox wait/poll operations.
///
/// Multiple event types can be set simultaneously. Event flags can be combined
/// with bitwise OR and tested with bitwise AND.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EventFlags(pub u32);

impl EventFlags {
    /// No events.
    pub const NONE: Self = Self(0);

    // Channel events (bits 0-2)
    /// Message available to receive.
    pub const CHANNEL_READABLE: Self = Self(1 << 0);
    /// Space available in send queue.
    pub const CHANNEL_WRITABLE: Self = Self(1 << 1);
    /// Peer closed their endpoint.
    pub const CHANNEL_CLOSED: Self = Self(1 << 2);

    // Process events (bit 3)
    /// Child process has exited.
    pub const PROCESS_EXITED: Self = Self(1 << 3);

    // Keyboard events (bit 4)
    /// Key event available (key data packed in bits 8-25).
    pub const KEYBOARD_KEY: Self = Self(1 << 4);

    /// Check if channel readable flag is set.
    #[inline]
    pub const fn is_channel_readable(self) -> bool {
        self.0 & Self::CHANNEL_READABLE.0 != 0
    }

    /// Check if channel writable flag is set.
    #[inline]
    pub const fn is_channel_writable(self) -> bool {
        self.0 & Self::CHANNEL_WRITABLE.0 != 0
    }

    /// Check if channel closed flag is set.
    #[inline]
    pub const fn is_channel_closed(self) -> bool {
        self.0 & Self::CHANNEL_CLOSED.0 != 0
    }

    /// Check if process exited flag is set.
    #[inline]
    pub const fn is_process_exited(self) -> bool {
        self.0 & Self::PROCESS_EXITED.0 != 0
    }

    /// Check if keyboard key flag is set.
    #[inline]
    pub const fn is_keyboard_key(self) -> bool {
        self.0 & Self::KEYBOARD_KEY.0 != 0
    }

    /// Combine flags with bitwise OR.
    #[inline]
    pub const fn or(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Test if any of the specified flags are set.
    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Test if all of the specified flags are set.
    #[inline]
    pub const fn contains_all(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

// Legacy event flag constants
// Channel events (bits 0-2)
/// Message available to receive.
pub const EVENT_CHANNEL_READABLE: u32 = EventFlags::CHANNEL_READABLE.0;
/// Space available in send queue.
pub const EVENT_CHANNEL_WRITABLE: u32 = EventFlags::CHANNEL_WRITABLE.0;
/// Peer closed their endpoint.
pub const EVENT_CHANNEL_CLOSED: u32 = EventFlags::CHANNEL_CLOSED.0;

// Process events (bit 3)
/// Child process has exited.
pub const EVENT_PROCESS_EXITED: u32 = EventFlags::PROCESS_EXITED.0;

// Keyboard events (bit 4)
/// Key event available. Key data is packed in bits 8-25:
/// - Bits 8-23: key code (16 bits)
/// - Bits 24-25: key value (0=release, 1=press, 2=repeat)
pub const EVENT_KEYBOARD_KEY: u32 = EventFlags::KEYBOARD_KEY.0;

// Keyboard event encoding helpers
/// Shift for key code in event flags.
pub const EVENT_KEY_CODE_SHIFT: u32 = 8;
/// Mask for key code (16 bits).
pub const EVENT_KEY_CODE_MASK: u32 = 0xFFFF;
/// Shift for key value in event flags.
pub const EVENT_KEY_VALUE_SHIFT: u32 = 24;
/// Mask for key value (2 bits).
pub const EVENT_KEY_VALUE_MASK: u32 = 0x3;

/// Encode a keyboard event into event flags.
#[inline]
pub const fn encode_key_event(code: u16, value: u8) -> u32 {
    EVENT_KEYBOARD_KEY
        | ((code as u32) << EVENT_KEY_CODE_SHIFT)
        | (((value as u32) & EVENT_KEY_VALUE_MASK) << EVENT_KEY_VALUE_SHIFT)
}

/// Decode key code from event flags.
#[inline]
pub const fn decode_key_code(flags: u32) -> u16 {
    ((flags >> EVENT_KEY_CODE_SHIFT) & EVENT_KEY_CODE_MASK) as u16
}

/// Decode key value from event flags.
#[inline]
pub const fn decode_key_value(flags: u32) -> u8 {
    ((flags >> EVENT_KEY_VALUE_SHIFT) & EVENT_KEY_VALUE_MASK) as u8
}

// =============================================================================
// Shared types
// =============================================================================

/// File stat structure shared between kernel and userspace
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FileStat {
    pub size: u64,
    pub is_dir: bool,
}

/// Maximum length of a directory entry name
pub const DIRENT_NAME_MAX: usize = 255;

/// Directory entry structure shared between kernel and userspace
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DirEntry {
    /// Length of the name (not including null terminator)
    pub name_len: u8,
    /// Whether this entry is a directory
    pub is_dir: bool,
    /// Entry name (not null-terminated, use name_len)
    pub name: [u8; DIRENT_NAME_MAX],
}

impl DirEntry {
    /// Get the entry name as a string slice
    pub fn name(&self) -> &str {
        // Safety: kernel only writes valid UTF-8
        unsafe { core::str::from_utf8_unchecked(&self.name[..self.name_len as usize]) }
    }
}

/// Header for startup messages sent from parent to child process.
///
/// The startup message is sent over HANDLE_PARENT immediately after spawn.
/// Layout after header:
/// - `[u16; arg_count]` - length of each argument string
/// - packed argument strings (no null terminators, use lengths above)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StartupMessageHeader {
    /// Protocol version (currently 1).
    pub version: u16,
    /// Number of argument strings.
    pub arg_count: u16,
    /// Number of environment variables (reserved for future use).
    pub env_count: u16,
    /// Reserved flags.
    pub flags: u16,
}

// =============================================================================
// Error codes
// =============================================================================

/// Error codes for resource operations.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// Operation completed successfully.
    Ok = 0,
    /// Resource not found.
    NotFound = 1,
    /// Invalid offset or position.
    InvalidOffset = 2,
    /// Resource is not readable.
    NotReadable = 3,
    /// Resource is not writable.
    NotWritable = 4,
    /// Resource is not seekable.
    NotSeekable = 5,
    /// Operation not supported.
    NotSupported = 6,
    /// Permission denied.
    PermissionDenied = 7,
    /// I/O error.
    IoError = 8,
    /// Would block (used internally, not returned to userspace).
    WouldBlock = 9,
    /// Invalid argument.
    InvalidArgument = 10,
    /// Protocol error (unexpected message type).
    Protocol = 11,
}

// =============================================================================
// Message types for message-passing interface
// =============================================================================

/// Message header for tagged messages (used for correlation).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MessageHeader {
    /// Message ID for request/response correlation.
    /// ID 0 is reserved for unsolicited events.
    pub id: u64,
    /// Message type discriminant.
    pub msg_type: u32,
    /// Reserved for future use.
    pub _reserved: u32,
}

// -----------------------------------------------------------------------------
// Block interface messages (files, disks, memory regions)
// -----------------------------------------------------------------------------

/// Block message types.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockMessageType {
    /// Read data at offset.
    Read = 0,
    /// Write data at offset.
    Write = 1,
    /// Get block size.
    Stat = 2,
    /// Sync buffered writes.
    Sync = 3,
}

/// Block read request.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BlockReadRequest {
    pub header: MessageHeader,
    pub offset: u64,
    pub len: u32,
    pub _pad: u32,
}

/// Block read response.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BlockReadResponse {
    pub header: MessageHeader,
    pub error: ErrorCode,
    pub len: u32,
    // Data follows in buffer
}

/// Block write request (data follows header).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BlockWriteRequest {
    pub header: MessageHeader,
    pub offset: u64,
    pub len: u32,
    pub _pad: u32,
    // Data follows
}

/// Block write response.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BlockWriteResponse {
    pub header: MessageHeader,
    pub error: ErrorCode,
    pub written: u32,
}

/// Block stat request.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BlockStatRequest {
    pub header: MessageHeader,
}

/// Block stat response.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BlockStatResponse {
    pub header: MessageHeader,
    pub error: ErrorCode,
    pub _pad: u32,
    pub size: u64,
}

// -----------------------------------------------------------------------------
// EventSource interface messages (keyboard, mouse, timers)
// -----------------------------------------------------------------------------

/// Event source message types.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventMessageType {
    /// Poll for an event.
    Poll = 0,
}

/// Event poll request.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EventPollRequest {
    pub header: MessageHeader,
}

/// Event types.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    /// No event available.
    None = 0,
    /// Key press/release.
    Key = 1,
    /// Mouse movement/button.
    Mouse = 2,
    /// Timer expiration.
    Timer = 3,
}

/// Key event data.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KeyEventData {
    /// Key code.
    pub code: u16,
    /// Padding.
    pub _pad: u16,
    /// Value: 0=release, 1=press, 2=repeat.
    pub value: u32,
}

/// Mouse event data.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MouseEventData {
    /// X movement delta.
    pub dx: i32,
    /// Y movement delta.
    pub dy: i32,
    /// Button state.
    pub buttons: u32,
    pub _pad: u32,
}

/// Event poll response.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EventPollResponse {
    pub header: MessageHeader,
    pub event_type: EventType,
    pub _pad: u32,
    // Event-specific data follows (KeyEventData, MouseEventData, etc.)
}

// -----------------------------------------------------------------------------
// Directory interface messages
// -----------------------------------------------------------------------------

/// Directory message types.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirMessageType {
    /// Get entry at index.
    Entry = 0,
    /// Get entry count.
    Count = 1,
}

/// Directory entry request.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DirEntryRequest {
    pub header: MessageHeader,
    pub index: u64,
}

/// Directory entry response.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DirEntryResponse {
    pub header: MessageHeader,
    pub error: ErrorCode,
    pub found: u32,
    pub is_dir: u32,
    pub name_len: u32,
    // Name follows
}

/// Directory count request.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DirCountRequest {
    pub header: MessageHeader,
}

/// Directory count response.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DirCountResponse {
    pub header: MessageHeader,
    pub error: ErrorCode,
    pub _pad: u32,
    pub count: u64,
}

// -----------------------------------------------------------------------------
// Process interface messages
// -----------------------------------------------------------------------------

/// Process message types.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessMessageType {
    /// Get process status.
    GetStatus = 0,
    /// Send signal.
    Signal = 1,
    /// Wait for exit.
    Wait = 2,
}

/// Process status request.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessStatusRequest {
    pub header: MessageHeader,
}

/// Process status response.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessStatusResponse {
    pub header: MessageHeader,
    pub error: ErrorCode,
    pub running: u32,
    pub exit_code: i32,
    pub _pad: u32,
}

/// Process signal request.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessSignalRequest {
    pub header: MessageHeader,
    pub signal: u32,
    pub _pad: u32,
}

/// Process signal response.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessSignalResponse {
    pub header: MessageHeader,
    pub error: ErrorCode,
    pub _pad: u32,
}

// -----------------------------------------------------------------------------
// CharacterOutput interface messages
// -----------------------------------------------------------------------------

/// Character output message types.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharOutMessageType {
    /// Write data.
    Write = 0,
    /// Flush output.
    Flush = 1,
}

/// Character output write request (data follows header).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CharOutWriteRequest {
    pub header: MessageHeader,
    pub len: u32,
    pub _pad: u32,
    // Data follows
}

/// Character output write response.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CharOutWriteResponse {
    pub header: MessageHeader,
    pub error: ErrorCode,
    pub written: u32,
}

/// Character output flush request.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CharOutFlushRequest {
    pub header: MessageHeader,
}

/// Character output flush response.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CharOutFlushResponse {
    pub header: MessageHeader,
    pub error: ErrorCode,
    pub _pad: u32,
}

// -----------------------------------------------------------------------------
// Surface interface types (for framebuffer, display)
// -----------------------------------------------------------------------------

/// Pixel format for surfaces.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 32-bit ARGB (alpha, red, green, blue)
    ARGB8888 = 0,
}

/// Surface info returned via pointer.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SurfaceInfoOut {
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub stride: u32,
}

/// Parameters for blit operation.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BlitParams {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub buffer_handle: u32,
}

/// Parameters for fill operation.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FillParams {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub color: u32,
}

/// Rectangle for flush operation.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SurfaceRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Parameters for updating window position, size, and visibility.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct UpdateParamsIn {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub visible: u32, // 0 = hidden, 1 = visible
}
