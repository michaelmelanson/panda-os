# Virtual Filesystem

This document describes the async VFS layer and filesystem drivers.

## Design Principles

1. **Everything is async** - All I/O operations are async fns. Synchronous implementations (like TarFs) return immediately-ready futures.

2. **Use `async-trait`** - The `async_trait` crate allows writing `async fn` directly in traits.

3. **Sync is just fast async** - No separate sync path. In-memory filesystems complete immediately; disk-backed ones yield until I/O completes.

## Architecture

```
              VFS (all async via async-trait)
    async fn open/read/write/seek/stat/readdir
                 /                   \
                v                     v
            TarFs                   Ext2Fs
       (immediate ready)      (yields on disk I/O)
                                      |
                                      v
                          BlockDevice (async)
                      async fn read_at/write_at
                                      |
                                      v
                          VirtioBlockDevice
                   (futures with DMA buffers, IRQ wakeup)
```

## VFS Traits

### File Trait

```rust
#[async_trait]
pub trait File: Send + Sync {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError>;
    async fn write(&mut self, buf: &[u8]) -> Result<usize, FsError>;
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, FsError>;
    async fn stat(&self) -> Result<FileStat, FsError>;
}
```

### Filesystem Trait

```rust
#[async_trait]
pub trait Filesystem: Send + Sync {
    async fn open(&self, path: &str) -> Result<Box<dyn File>, FsError>;
    async fn stat(&self, path: &str) -> Result<FileStat, FsError>;
    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError>;
}
```

### BlockDevice Trait

```rust
#[async_trait]
pub trait BlockDevice: Send + Sync {
    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, BlockError>;
    async fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, BlockError>;
    fn size(&self) -> u64;
    fn sector_size(&self) -> u32;
    async fn sync(&self) -> Result<(), BlockError>;
}
```

## Mount System

Filesystems are mounted at path prefixes. The VFS resolves paths by finding the longest matching mount point:

```rust
pub fn mount(path: &str, fs: Arc<dyn Filesystem>);
pub async fn open(path: &str) -> Result<Box<dyn File>, FsError>;
pub async fn stat(path: &str) -> Result<FileStat, FsError>;
pub async fn readdir(path: &str) -> Result<Vec<DirEntry>, FsError>;
```

## Virtio Block Driver

The virtio block driver uses custom futures that own their DMA buffers.

### VirtioReadFuture / VirtioWriteFuture

Each async I/O operation creates a future that:
1. Allocates a DMA buffer
2. Submits the request to the virtio queue
3. Registers a waker with the device
4. Returns `Pending` until the IRQ handler wakes it
5. On completion, copies data from DMA buffer to user buffer

```rust
struct VirtioReadFuture {
    device: Arc<Spinlock<VirtioBlockDeviceInner>>,
    sector: u64,
    buf_ptr: *mut u8,
    buf_len: usize,
    dma_buffer: Option<DmaBuffer>,
    request_header: BlockRequest,
    response_status: BlockResponse,
    state: AsyncReadState,
}

enum AsyncReadState {
    NotSubmitted,
    Submitted { token: VirtioToken },
    Completed { token: VirtioToken },
}
```

### Device State

The device tracks wakers and completed tokens:

```rust
struct VirtioBlockDeviceInner {
    device: VirtIOBlk<VirtioHal, MsixPciTransport>,
    async_wakers: BTreeMap<VirtioToken, TaskWaker>,
    completed_tokens: BTreeSet<VirtioToken>,
    // ...
}
```

### IRQ Handler

The IRQ handler marks tokens as completed and wakes futures:

```rust
pub fn process_completions(&mut self) {
    self.device.ack_interrupt();
    
    while let Some(token) = self.peek_completed_token() {
        if let Some(waker) = self.async_wakers.remove(&token) {
            self.completed_tokens.insert(token);
            waker.wake();
            break;
        }
    }
}
```

### Unaligned I/O

The `VirtioBlockDevice` wrapper handles byte-level access with automatic sector alignment:

- **Aligned path**: Direct read/write of whole sectors
- **Unaligned path**: Read-modify-write for partial sector access

## Ext2 Filesystem

### On-Disk Structures

Key constants:
- Magic number: `0xEF53`
- Root inode: 2
- Superblock offset: 1024 bytes

Structures:
- **Superblock** (1024 bytes): Filesystem metadata, block size, inode count
- **Block Group Descriptor** (32 bytes): Block/inode bitmap locations, inode table
- **Inode** (128+ bytes): Mode, size, block pointers (12 direct + 3 indirect levels)
- **Directory Entry**: Inode number, record length, name length, file type, name

### Block Indirection

Inode block pointers:
- 0-11: Direct blocks
- 12: Single indirect (block of pointers)
- 13: Double indirect (block of single indirect blocks)
- 14: Triple indirect (block of double indirect blocks)

```rust
pub async fn get_block_number(
    device: &dyn BlockDevice,
    block_pointers: &[u32; 15],
    block_size: u32,
    file_block: u32,
) -> Result<u32, FsError>;
```

### Ext2Fs

```rust
pub struct Ext2Fs {
    device: Arc<dyn BlockDevice>,
    block_size: u32,
    inode_size: u32,
    inodes_per_group: u32,
    block_groups: Vec<BlockGroupDescriptor>,
}

impl Ext2Fs {
    pub async fn mount(device: Arc<dyn BlockDevice>) -> Result<Arc<Self>, &'static str>;
    pub async fn read_inode(&self, ino: u32) -> Result<Inode, FsError>;
    pub async fn lookup(&self, path: &str) -> Result<u32, FsError>;
    pub async fn get_block(&self, inode: &Inode, file_block: u32) -> Result<u32, FsError>;
}
```

### Ext2File

```rust
pub struct Ext2File {
    device: Arc<dyn BlockDevice>,
    inode: Inode,
    block_size: u32,
    size: u64,
    pos: u64,
}
```

Implements `File` trait with async read/seek/stat. Read handles:
- Sparse holes (block number 0 returns zeros)
- Multi-block reads across block boundaries
- Indirect block lookups

## BlockDeviceFile

Wraps a `BlockDevice` as a `File` for raw block device access through the VFS:

```rust
pub struct BlockDeviceFile {
    device: Arc<dyn BlockDevice>,
    pos: u64,
}
```

## Synchronous Polling

For testing or contexts without a scheduler, `poll_immediate` polls a future once:

```rust
pub fn poll_immediate<T>(future: Pin<&mut impl Future<Output = T>>) -> Option<T>;
```

Returns `Some(result)` if the future completes immediately (e.g., TarFs), `None` if pending.

## Files

| File | Description |
|------|-------------|
| `vfs/mod.rs` | VFS traits, mount system, BlockDeviceFile |
| `vfs/tarfs.rs` | In-memory tar filesystem |
| `vfs/ext2/mod.rs` | Ext2 filesystem implementation |
| `vfs/ext2/file.rs` | Ext2File implementation |
| `vfs/ext2/structs.rs` | On-disk structures |
| `resource/block.rs` | BlockDevice trait |
| `devices/virtio_block.rs` | Virtio block driver with async futures |
