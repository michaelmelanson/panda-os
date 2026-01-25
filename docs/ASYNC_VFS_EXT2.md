# Async VFS and Ext2 Filesystem

This document describes the design for making the VFS layer fully async and implementing a read-only ext2 filesystem driver.

## Design Principles

1. **Everything is async** - All I/O operations are async fns. Synchronous implementations (like TarFs) simply return immediately-ready futures.

2. **Use `async-trait`** - Allows writing `async fn` directly in traits without manual boxing boilerplate.

3. **Sync is just fast async** - No separate sync path. In-memory filesystems complete immediately; disk-backed ones yield until I/O completes.

4. **Processes block on futures** - Syscalls that do async I/O store the future in the process and yield. The scheduler polls the future when the process is woken.

## Architecture

```
                         Userspace
    file::read(fd, buf) -> syscall
                            |
                            v
                     Syscall Handler
    Creates future, polls once, yields if pending
                            |
                            v
                        Scheduler
    Polls pending future when process is woken
    On Ready: writes result to rax, returns to userspace
    On Pending: process stays blocked
                            |
                            v
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
```

## Process State Model

```rust
struct Process {
    state: ProcessState,
    pending_syscall: Option<PendingSyscall>,
    // ... existing fields
}

enum ProcessState {
    Runnable,  // Ready to run (either return to userspace or poll future)
    Running,   // Currently executing
    Blocked,   // Waiting for waker (future returned Pending)
}

struct PendingSyscall {
    future: Pin<Box<dyn Future<Output = isize> + Send>>,
}
```

When a future returns `Pending`, it has registered a waker with some interrupt handler (e.g., virtio-blk IRQ). When that interrupt fires, the waker marks the process `Runnable`. The scheduler then polls the future again.

## Async Syscall Flow

### 1. Userspace calls syscall
```
User: syscall(READ, fd, buf, len)
  -> Kernel entry via syscall instruction
```

### 2. Syscall handler creates future, polls once
```rust
fn handle_read_syscall(fd, buf_ptr, buf_len, ctx: &mut SyscallContext) {
    let file = ctx.process.handles.get(fd);
    let buf = unsafe { slice::from_raw_parts_mut(buf_ptr, buf_len) };
    
    let mut future = Box::pin(async move {
        file.read(buf).await.map(|n| n as isize).unwrap_or(-1)
    });
    
    // OPTIMIZATION: Poll once immediately - sync ops complete here
    let waker = create_process_waker(ctx.process.id);
    let mut cx = Context::from_waker(&waker);
    
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(result) => {
            // Completed immediately (e.g., TarFs)
            return_to_userspace_with_result(result);
        }
        Poll::Pending => {
            // Async I/O in progress
            ctx.process.pending_syscall = Some(PendingSyscall { future });
            ctx.process.state = Blocked;
            scheduler::yield_current();
        }
    }
}
```

### 3. Scheduler polls when process is woken
```rust
fn run_next_process() {
    let process = pick_runnable_process();
    
    if let Some(ref mut pending) = process.pending_syscall {
        let waker = create_process_waker(process.id);
        let mut cx = Context::from_waker(&waker);
        
        match pending.future.as_mut().poll(&mut cx) {
            Poll::Ready(result) => {
                process.pending_syscall = None;
                return_from_syscall(process.ip, process.sp, result as u64);
            }
            Poll::Pending => {
                process.state = Blocked;
                run_next_process();
            }
        }
    } else {
        return_from_syscall(process.ip, process.sp, process.saved_rax);
    }
}
```

### 4. IRQ wakes the process
```rust
fn virtio_blk_irq_handler() {
    for completed_request in completed {
        completed_request.waker.wake();
    }
}

impl Wake for ProcessWaker {
    fn wake(self: Arc<Self>) {
        scheduler::mark_runnable(self.process_id);
    }
}
```

## Async VFS Traits

```rust
use async_trait::async_trait;

#[async_trait]
pub trait File: Send + Sync {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError>;
    async fn write(&mut self, buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::NotWritable)
    }
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, FsError>;
    async fn stat(&self) -> Result<FileStat, FsError>;
}

#[async_trait]
pub trait Filesystem: Send + Sync {
    async fn open(&self, path: &str) -> Result<Box<dyn File>, FsError>;
    async fn stat(&self, path: &str) -> Result<FileStat, FsError>;
    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError>;
}
```

## Async BlockDevice Trait

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

## Virtio Block Driver Changes

The current driver stores pending requests in the device. The new design has futures own their DMA buffers:

```rust
struct VirtioReadFuture {
    device: Arc<Spinlock<VirtioBlockDevice>>,
    sector: u64,
    buf_ptr: *mut u8,
    buf_len: usize,
    dma_buffer: Option<DmaBuffer>,  // Owned by future
    token: Option<u16>,
    state: ReadState,
}

enum ReadState {
    NotSubmitted,
    Submitted,
    Completed,
}
```

The device just tracks wakers by token:

```rust
pub struct VirtioBlockDevice {
    // ... existing fields ...
    waiting_wakers: BTreeMap<u16, Waker>,
    completed_tokens: BTreeSet<u16>,
}
```

IRQ handler marks tokens complete and wakes futures:

```rust
pub fn process_completions(&mut self) {
    while let Some(token) = self.device.peek_used() {
        self.completed_tokens.insert(token);
        if let Some(waker) = self.waiting_wakers.remove(&token) {
            waker.wake();
        }
    }
}
```

## Ext2 On-Disk Structures

Key constants:
- Magic number: `0xEF53`
- Root inode: 2
- Superblock offset: 1024 bytes

Structures:
- **Superblock** (1024 bytes): Filesystem metadata, block size, inode count
- **Block Group Descriptor** (32 bytes): Block/inode bitmap locations, inode table
- **Inode** (128+ bytes): Mode, size, block pointers (12 direct + 3 indirect levels)
- **Directory Entry**: Inode number, record length, name length, file type, name

## Ext2Fs Implementation

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

## Demonstration

To verify the async VFS and ext2 work end-to-end:
1. The terminal binary is placed in the ext2 filesystem
2. On boot, the ext2 filesystem is mounted
3. The terminal is launched from the ext2 mount point
4. A userspace test verifies files can be read from ext2

## Implementation Phases

### Phase A: Async Infrastructure
1. Add `async-trait` to Cargo.toml
2. Add `pending_syscall` to Process struct
3. Update scheduler to poll pending futures
4. Create process waker infrastructure

### Phase B: Async Block Device
5. Convert `BlockDevice` trait to async
6. Implement async virtio-blk with `VirtioReadFuture`
7. Update IRQ handler to wake futures

### Phase C: Async VFS
8. Convert `File` and `Filesystem` traits to async
9. Update TarFs with async trait implementation
10. Update resource scheme for async File

### Phase D: Ext2 Filesystem
11. Create ext2 on-disk structures
12. Implement Ext2Fs with async mount/lookup/read
13. Implement Ext2File with async read/seek

### Phase E: Integration
14. Update syscall handlers for async futures
15. Add ext2 mount support at boot
16. Move terminal to ext2, verify boot from ext2
17. Run all tests to verify no regressions

## Build System Changes

Create ext2 test image:
```makefile
EXT2_IMAGE = build/test.ext2

$(EXT2_IMAGE):
    dd if=/dev/zero of=$(EXT2_IMAGE) bs=1M count=10
    mkfs.ext2 -F $(EXT2_IMAGE)
    # Mount and populate with test files
    # Copy terminal binary into image
```

Add ext2 disk to QEMU:
```makefile
-drive file=$(EXT2_IMAGE),format=raw,if=none,id=ext2disk \
-device virtio-blk-pci,drive=ext2disk
```

## Testing

### Kernel Tests
- Superblock magic validation
- Mount ext2 filesystem
- Read root directory
- Read files (small, large, nested paths)
- Seek operations
- Error handling (not found)

### Userspace Tests
- Open and read files via VFS
- Directory listing
- Multi-block file reads
- Seek operations
- Terminal launch from ext2 mount
