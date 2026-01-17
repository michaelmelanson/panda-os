//! Shared ABI definitions between kernel and userspace.
//!
//! This crate contains syscall numbers, constants, and shared types
//! that both the kernel and userspace need to agree on.

#![no_std]

// =============================================================================
// Syscall numbers
// =============================================================================

/// The unified send syscall - all operations go through this
pub const SYSCALL_SEND: usize = 0x30;

// =============================================================================
// Well-known handles
// =============================================================================

/// Handle to the current process (Process resource)
pub const HANDLE_SELF: u32 = 0;

/// Handle to the system environment (Environment resource)
pub const HANDLE_ENVIRONMENT: u32 = 1;

// =============================================================================
// Operation codes
// =============================================================================

// File operations (0x1_0000 - 0x1_FFFF)
/// Read from file: (buf_ptr, buf_len) -> bytes_read
pub const OP_FILE_READ: u32 = 0x1_0000;
/// Write to file: (buf_ptr, buf_len) -> bytes_written
pub const OP_FILE_WRITE: u32 = 0x1_0001;
/// Seek in file: (offset_lo, offset_hi, whence) -> new_position
pub const OP_FILE_SEEK: u32 = 0x1_0002;
/// Get file stats: (stat_ptr) -> 0 or error
pub const OP_FILE_STAT: u32 = 0x1_0003;
/// Close file: () -> 0 or error
pub const OP_FILE_CLOSE: u32 = 0x1_0004;
/// Read directory entry: (entry_ptr) -> 1 if entry read, 0 if end of directory, negative on error
pub const OP_FILE_READDIR: u32 = 0x1_0005;

// Process operations (0x2_0000 - 0x2_FFFF)
/// Yield execution: () -> 0
pub const OP_PROCESS_YIELD: u32 = 0x2_0000;
/// Exit process: (code) -> !
pub const OP_PROCESS_EXIT: u32 = 0x2_0001;
/// Get process ID: () -> pid
pub const OP_PROCESS_GET_PID: u32 = 0x2_0002;
/// Wait for child: () -> exit_code or error
pub const OP_PROCESS_WAIT: u32 = 0x2_0003;
/// Signal process: (signal) -> 0 or error
pub const OP_PROCESS_SIGNAL: u32 = 0x2_0004;
/// Set program break: (new_brk) -> current_brk or error
/// If new_brk is 0, returns current break without changing it.
/// Pages are allocated on demand via page faults.
pub const OP_PROCESS_BRK: u32 = 0x2_0005;

// Userspace buffer region constants
/// Base address of the userspace buffer region.
/// Buffers are shared memory regions for zero-copy I/O.
/// Must be in userspace PML4 range (entries 20-22) and canonical (bit 47 = 0).
pub const BUFFER_BASE: usize = 0xaff_0000_0000;
/// Maximum size of the userspace buffer region (4 GB virtual address space).
pub const BUFFER_MAX_SIZE: usize = 0x1_0000_0000;

// Userspace stack region constants
/// Base address of the userspace stack region.
/// Stack grows downward from STACK_BASE + STACK_MAX_SIZE.
/// Must be in userspace PML4 range (entries 20-22) and canonical (bit 47 = 0).
pub const STACK_BASE: usize = 0xb00_0000_0000;
/// Maximum size of the userspace stack (16 MB virtual address space).
/// Actual physical memory is allocated on demand via page faults.
pub const STACK_MAX_SIZE: usize = 0x100_0000;

// Userspace heap region constants
/// Base address of the userspace heap region (after stack).
/// Must be in userspace PML4 range (entries 20-22) and canonical (bit 47 = 0).
pub const HEAP_BASE: usize = 0xb00_0100_0000;
/// Maximum size of the userspace heap (1 TB virtual address space)
/// Actual physical memory is allocated on demand via page faults.
pub const HEAP_MAX_SIZE: usize = 0x100_0000_0000;

// Environment operations (0x3_0000 - 0x3_FFFF)
/// Open file: (path_ptr, path_len, flags) -> handle
pub const OP_ENVIRONMENT_OPEN: u32 = 0x3_0000;
/// Spawn process: (path_ptr, path_len) -> process_handle
pub const OP_ENVIRONMENT_SPAWN: u32 = 0x3_0001;
/// Log message: (msg_ptr, msg_len) -> 0
pub const OP_ENVIRONMENT_LOG: u32 = 0x3_0002;
/// Get time: () -> timestamp
pub const OP_ENVIRONMENT_TIME: u32 = 0x3_0003;
/// Open directory: (path_ptr, path_len) -> dir_handle or error
pub const OP_ENVIRONMENT_OPENDIR: u32 = 0x3_0004;

// Buffer operations (0x4_0000 - 0x4_FFFF)
/// Allocate a shared buffer: (size, info_ptr) -> buffer_handle or error
/// If info_ptr is non-zero, writes BufferAllocInfo (addr, size) to that address.
pub const OP_BUFFER_ALLOC: u32 = 0x4_0000;
/// Resize a buffer: (buffer_handle, new_size, info_ptr) -> 0 or error
/// If info_ptr is non-zero, writes BufferAllocInfo (addr, size) to that address.
pub const OP_BUFFER_RESIZE: u32 = 0x4_0002;
/// Free a buffer: (buffer_handle) -> 0 or error
pub const OP_BUFFER_FREE: u32 = 0x4_0003;

/// Buffer allocation info returned via pointer in BUFFER_ALLOC.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BufferAllocInfo {
    pub addr: usize,
    pub size: usize,
}

// Buffer-based file operations (0x5_0000 - 0x5_FFFF)
/// Read from file into buffer: (file_handle, buffer_handle) -> bytes_read
pub const OP_FILE_READ_BUFFER: u32 = 0x5_0000;
/// Write from buffer to file: (file_handle, buffer_handle, len) -> bytes_written
pub const OP_FILE_WRITE_BUFFER: u32 = 0x5_0001;

// Surface operations (0x6_0000 - 0x6_FFFF)
/// Get surface info: (info_ptr) -> 0 or error
pub const OP_SURFACE_INFO: u32 = 0x6_0000;
/// Blit pixels to surface: (params_ptr) -> 0 or error
pub const OP_SURFACE_BLIT: u32 = 0x6_0001;
/// Fill rectangle with solid color: (params_ptr) -> 0 or error
pub const OP_SURFACE_FILL: u32 = 0x6_0002;
/// Flush surface updates: (rect_ptr) -> 0 or error (rect_ptr can be 0 for full flush)
pub const OP_SURFACE_FLUSH: u32 = 0x6_0003;

// =============================================================================
// Constants
// =============================================================================

// Seek whence values
pub const SEEK_SET: usize = 0;
pub const SEEK_CUR: usize = 1;
pub const SEEK_END: usize = 2;

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
