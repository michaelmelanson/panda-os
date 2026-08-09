# Filesystems

Since roadmap M5 the filesystem is a userspace service. This document
describes where it lives, how it talks to the rest of the system, and what
the kernel kept.

## Where things live

```
    client process
   open("file:/mnt/x")            open("initrd:/init")
          |                              |
          v                              |
   kernel scheme registry                |
   (resource/scheme.rs)                  |
          |                              v
          |                     InitrdScheme -> TarFs
          |                     (kernel, read-only, in memory)
          v
   UserSchemeProvider  -- scheme_protocol frames -->  fs service
                                                 (userspace/services/fs)
                                                          |
                                          MountTable: /initrd, /mnt
                                                     /            \
                                                    v              v
                                              InitrdFs           Ext2Fs
                                          (client of initrd:)      |
                                                                   v
                                                        SchemeBlockDevice
                                                     (client of block:)
                                                                   |
                                                                   v
                                                    virtio-blk driver process
                                                  (userspace/drivers/virtio-blk)
```

The kernel keeps exactly one filesystem: [`TarFs`], the in-memory ustar
archive UEFI hands it at boot, served by the `initrd:` scheme. That is what
loads `init`, the drivers, and the filesystem service itself — all of which
have to be readable before any filesystem is mounted.

Everything else — the mount table, path canonicalisation, ext2, and the
`file:` scheme — is the `fs` service, an ordinary process with no special
privileges. It registers `file:` with `OP_SCHEME_REGISTER` exactly as the
virtio drivers register `block:`/`display:`/`keyboard:`.

## Startup and the readiness handshake

There is no mount syscall. The service:

1. mounts `/initrd` (proxying the kernel's `initrd:` scheme) and `/mnt`
   (ext2 on `block:/pci/storage/0`);
2. registers the `file:` scheme;
3. sends `b"ready"` on its spawn channel.

Because `file:` resolves to nothing until step 2, anything that has seen the
handshake (`libpanda::process::wait_ready`) is guaranteed to see a fully
mounted filesystem. `init` spawns the service from `initrd:/fs` right after
the initrd drivers and waits for that message before touching any `file:`
URI. A test that stands in for `init` does the same.

A failed ext2 mount is not fatal: the service still serves `/initrd`, and
logs why. Several tests run with no block device attached at all.

Holding the `block:` handle open for the mount's lifetime *is* the exclusive
claim on the device — the virtio-blk driver refuses a second concurrent
`Open` with `Busy` — so a raw `block:` open while ext2 is mounted fails, as
`claim_test` checks.

## Sync in userspace, async in the kernel

The service's interfaces are synchronous. They were async in the kernel
because they ran inside the kernel task executor, where a block read had to
yield rather than stall the kernel; in a process a block read is a blocking
`OP_FILE_READ` syscall on the `block:` handle, and the kernel runs everything
else while it is blocked. Being synchronous is also what lets the mount
table and ext2 be unit-tested on the host (`cargo test -p fs
--no-default-features`, run by `make unit-test`).

The kernel's remaining `Filesystem`/`File` traits are still async, since the
executor is what drives them. Nothing crosses between the two as a trait
object — the two sides talk in `panda_abi::scheme_protocol` frames.

## Interfaces

### Filesystem and File (`userspace/services/fs/src/fs.rs`)

```rust
pub trait Filesystem: Send + Sync {
    // Required (read operations):
    fn open(&self, path: &str) -> Result<Box<dyn File>, FsError>;
    fn stat(&self, path: &str) -> Result<FileStat, FsError>;
    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError>;

    // Optional (write operations — default to Err(FsError::ReadOnlyFs)):
    fn create(&self, path: &str, mode: u16) -> Result<Box<dyn File>, FsError>;
    fn unlink(&self, path: &str) -> Result<(), FsError>;
    fn mkdir(&self, path: &str, mode: u16) -> Result<(), FsError>;
    fn rmdir(&self, path: &str) -> Result<(), FsError>;
    fn truncate(&self, path: &str, size: u64) -> Result<(), FsError>;
    fn sync(&self) -> Result<(), FsError>;
}

pub trait File: Send + Sync {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError>;
    fn write(&mut self, buf: &[u8]) -> Result<usize, FsError>;
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, FsError>;
    fn stat(&self) -> Result<FileStat, FsError>;
}
```

The kernel's copy (`panda-kernel/src/vfs/mod.rs`) has only the read half and
is async: `TarFs` is read-only, so `write`/`create`/`unlink`/`mkdir`/`rmdir`/
`truncate`/`sync` had no implementor left in the kernel at all.

### BlockDevice (`userspace/services/fs/src/block.rs`)

```rust
pub trait BlockDevice: Send + Sync {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, BlockError>;
    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, BlockError>;
    fn size(&self) -> u64;
    fn sector_size(&self) -> u32;
    fn sync(&self) -> Result<(), BlockError>;
}
```

The only implementor is `SchemeBlockDevice`, which is an open `block:`
handle plus `OP_FILE_SEEK`/`READ`/`WRITE`. The kernel has no `BlockDevice`
trait any more.

### MountTable (`userspace/services/fs/src/mount.rs`)

```rust
pub struct MountTable { /* ... */ }

impl MountTable {
    pub fn mount(&mut self, path: &str, fs: Arc<dyn Filesystem>);
    pub fn open(&self, path: &str) -> Result<Box<dyn File>, FsError>;
    pub fn stat(&self, path: &str) -> Result<FileStat, FsError>;
    pub fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError>;
    pub fn create(&self, path: &str, mode: u16) -> Result<Box<dyn File>, FsError>;
    pub fn unlink(&self, path: &str) -> Result<(), FsError>;
    pub fn mkdir(&self, path: &str, mode: u16) -> Result<(), FsError>;
    pub fn rmdir(&self, path: &str) -> Result<(), FsError>;
    pub fn truncate(&self, path: &str, size: u64) -> Result<(), FsError>;
    pub fn sync(&self, path: &str) -> Result<(), FsError>;
}
```

Paths resolve to the longest matching mount point, and are canonicalised
first so `..` cannot escape a mount. It is an owned table, not kernel-global
state — the service owns exactly one.

## Wire protocol

`file:` uses the same `panda_abi::scheme_protocol` every scheme provider
does. M5 added five request kinds, because the syscalls behind them used to
call the kernel's `vfs::` functions directly and had nothing left to call:

| Kind | Serves | Payload |
|------|--------|---------|
| `MSG_STAT` | `OP_FILE_STAT` | `resource_id` -> `(size, is_dir)` |
| `MSG_CREATE` | `OP_DIRECTORY_CREATE` | `(mode, path)` -> `resource_id` |
| `MSG_UNLINK` | `OP_DIRECTORY_UNLINK` | `path` |
| `MSG_MKDIR` | `OP_DIRECTORY_MKDIR` | `(mode, path)` |
| `MSG_RMDIR` | `OP_DIRECTORY_RMDIR` | `path` |

`MSG_STAT` is separate from the older `MSG_SIZE` because `MSG_SIZE` answers
only a byte count — `block:`, the reason it exists, has no notion of a
directory, while `OP_FILE_STAT`'s `FileStat` carries `is_dir`.

The four path operations are path-based rather than `resource_id`-based
because that is the shape of the syscalls: the directory handle supplies a
directory, which the kernel joins with the caller's entry name. A directory
handle therefore carries the full URI it was listed from (`"file:/mnt"`),
not a bare path, so the operation reaches the provider that owns the scheme.

One frame carries at most `MAX_TRANSFER_SIZE` (4000) bytes, so the kernel
splits a larger `OP_FILE_WRITE` across several round trips rather than
truncating it.

## Error types

`FsError` covers all filesystem error conditions, and is mapped onto
`panda_abi::ErrorCode` at the wire boundary:

| Variant | Description |
|---------|-------------|
| `NotFound` | Path not found |
| `InvalidOffset` | Invalid seek offset |
| `NotReadable` | Resource is not readable |
| `NotWritable` | Resource is not writable |
| `NotSeekable` | Resource is not seekable |
| `NoSpace` | Filesystem is full |
| `AlreadyExists` | File or directory already exists at path |
| `NotEmpty` | Directory is not empty (for rmdir) |
| `IsDirectory` | Operation not valid on directories |
| `NotDirectory` | Expected a directory but found a file |
| `ReadOnlyFs` | Filesystem is mounted read-only |
| `IoError` | Block device I/O failure |

## Ext2

### On-disk structures

Key constants:
- Magic number: `0xEF53`
- Root inode: 2
- Superblock offset: 1024 bytes

Structures:
- **Superblock** (1024 bytes): Filesystem metadata, block size, inode count
- **Block Group Descriptor** (32 bytes): Block/inode bitmap locations, inode table
- **Inode** (128+ bytes): Mode, size, block pointers (12 direct + 3 indirect levels)
- **Directory Entry**: Inode number, record length, name length, file type, name

Every field read off disk is validated before use (`Superblock::validate`),
so a corrupted or malicious image cannot cause overflow, division by zero,
or out-of-bounds reads.

### Block indirection

Inode block pointers:
- 0-11: Direct blocks
- 12: Single indirect (block of pointers)
- 13: Double indirect (block of single indirect blocks)
- 14: Triple indirect (block of double indirect blocks)

### Allocation

Blocks and inodes are allocated first-fit from the per-group bitmaps.
`InodeGuard`/`BlockGuard` are RAII guards that free an allocation if a
multi-step operation (`mkdir`, `create`) fails before its commit point.

The kernel version serialised the bitmap read-modify-write cycle with an
async mutex, because that cycle spanned `.await` points at which another
kernel task could interleave. Nothing can interleave in the service: it is
single-threaded and each cycle runs straight through, so no such lock
exists.

### Ext2File

Reads handle sparse holes (block number 0 returns zeros), multi-block reads,
and indirect lookups. Writes handle block allocation, read-modify-write for
partial blocks, file extension, indirect block allocation at all three
levels, and inode metadata updates.

## Driving kernel VFS futures before the scheduler runs

The kernel's `initrd:` operations are async. At runtime they are driven by
the kernel task executor via syscall futures. During early boot, before the
scheduler and executor exist, `executor::block_on_immediate(future)` polls a
future once with a no-op waker and panics if it is not immediately ready —
valid because `TarFs` reads out of memory and always resolves on the first
poll. Boot uses this to load the init binary (`initrd:/init`) through the
ordinary `resource::load_binary` path.

## Files

| File | Description |
|------|-------------|
| `userspace/services/fs/src/main.rs` | The service: registers `file:`, serves requests |
| `userspace/services/fs/src/mount.rs` | Mount table and path canonicalisation (+ host tests) |
| `userspace/services/fs/src/fs.rs` | `Filesystem`/`File`/`FsError` interfaces |
| `userspace/services/fs/src/block.rs` | `BlockDevice` interface |
| `userspace/services/fs/src/schemes.rs` | `block:` and `initrd:` clients |
| `userspace/services/fs/src/ext2/` | Ext2 implementation (+ host tests) |
| `panda-kernel/src/vfs/mod.rs` | The kernel's remaining read-only `Filesystem`/`File` traits |
| `panda-kernel/src/vfs/tarfs.rs` | In-memory tar filesystem (backs `initrd:`) |
| `panda-kernel/src/resource/initrd.rs` | The `initrd:` scheme handler |
| `userspace/drivers/virtio-blk/src/main.rs` | The `block:` provider |
