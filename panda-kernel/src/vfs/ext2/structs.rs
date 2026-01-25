//! Ext2 on-disk structures.
//!
//! These structures match the ext2 specification for little-endian systems.

/// Ext2 magic number in the superblock.
pub const EXT2_SUPER_MAGIC: u16 = 0xEF53;

/// Root directory inode number (always 2 in ext2).
pub const EXT2_ROOT_INO: u32 = 2;

/// Superblock offset from start of device (in bytes).
pub const SUPERBLOCK_OFFSET: u64 = 1024;

// Inode mode type mask
pub const S_IFMT: u16 = 0xF000;
/// Regular file
pub const S_IFREG: u16 = 0x8000;
/// Directory
pub const S_IFDIR: u16 = 0x4000;
/// Symbolic link
pub const S_IFLNK: u16 = 0xA000;

// Directory entry file types
/// Regular file
pub const FT_REG_FILE: u8 = 1;
/// Directory
pub const FT_DIR: u8 = 2;
/// Symbolic link
pub const FT_SYMLINK: u8 = 7;

/// Ext2 superblock structure.
///
/// Located at byte offset 1024 from the start of the device.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Superblock {
    /// Total number of inodes in the filesystem
    pub inodes_count: u32,
    /// Total number of blocks in the filesystem
    pub blocks_count: u32,
    /// Number of blocks reserved for the superuser
    pub reserved_blocks_count: u32,
    /// Number of free blocks
    pub free_blocks_count: u32,
    /// Number of free inodes
    pub free_inodes_count: u32,
    /// Block number of the first data block (superblock)
    pub first_data_block: u32,
    /// Block size = 1024 << log_block_size
    pub log_block_size: u32,
    /// Fragment size (obsolete, usually same as block size)
    pub log_frag_size: u32,
    /// Number of blocks per block group
    pub blocks_per_group: u32,
    /// Number of fragments per block group (obsolete)
    pub frags_per_group: u32,
    /// Number of inodes per block group
    pub inodes_per_group: u32,
    /// Last mount time
    pub mtime: u32,
    /// Last write time
    pub wtime: u32,
    /// Mount count since last fsck
    pub mnt_count: u16,
    /// Maximum mount count before fsck
    pub max_mnt_count: u16,
    /// Magic number (0xEF53)
    pub magic: u16,
    /// Filesystem state
    pub state: u16,
    /// What to do on error
    pub errors: u16,
    /// Minor revision level
    pub minor_rev_level: u16,
    /// Last fsck time
    pub lastcheck: u32,
    /// Maximum time between fscks
    pub checkinterval: u32,
    /// Creator OS
    pub creator_os: u32,
    /// Revision level (0 = original, 1 = dynamic)
    pub rev_level: u32,
    /// Default UID for reserved blocks
    pub def_resuid: u16,
    /// Default GID for reserved blocks
    pub def_resgid: u16,
    // --- EXT2_DYNAMIC_REV (rev_level >= 1) fields ---
    /// First non-reserved inode
    pub first_ino: u32,
    /// Inode structure size
    pub inode_size: u16,
    /// Block group number of this superblock
    pub block_group_nr: u16,
    /// Compatible feature set
    pub feature_compat: u32,
    /// Incompatible feature set
    pub feature_incompat: u32,
    /// Read-only compatible feature set
    pub feature_ro_compat: u32,
    /// 128-bit UUID
    pub uuid: [u8; 16],
    /// Volume name
    pub volume_name: [u8; 16],
    /// Last mounted path
    pub last_mounted: [u8; 64],
    /// Compression algorithm bitmap
    pub algo_bitmap: u32,
    // Padding to 1024 bytes
    pub _padding: [u8; 820],
}

impl Superblock {
    /// Calculate block size from log_block_size.
    pub fn block_size(&self) -> u32 {
        1024 << self.log_block_size
    }

    /// Get inode size (128 for rev0, variable for rev1+).
    pub fn inode_size(&self) -> u32 {
        if self.rev_level >= 1 {
            self.inode_size as u32
        } else {
            128
        }
    }

    /// Calculate number of block groups.
    pub fn block_group_count(&self) -> u32 {
        self.blocks_count.div_ceil(self.blocks_per_group)
    }
}

/// Block group descriptor.
///
/// Located in the block group descriptor table, which follows the superblock.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BlockGroupDescriptor {
    /// Block number of block bitmap
    pub block_bitmap: u32,
    /// Block number of inode bitmap
    pub inode_bitmap: u32,
    /// Block number of first inode table block
    pub inode_table: u32,
    /// Number of free blocks in this group
    pub free_blocks_count: u16,
    /// Number of free inodes in this group
    pub free_inodes_count: u16,
    /// Number of directories in this group
    pub used_dirs_count: u16,
    /// Padding
    pub pad: u16,
    /// Reserved
    pub reserved: [u32; 3],
}

/// Inode structure.
///
/// Describes a file, directory, or other filesystem object.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Inode {
    /// File mode (type and permissions)
    pub mode: u16,
    /// Owner user ID
    pub uid: u16,
    /// File size in bytes (lower 32 bits)
    pub size: u32,
    /// Last access time
    pub atime: u32,
    /// Creation time
    pub ctime: u32,
    /// Last modification time
    pub mtime: u32,
    /// Deletion time
    pub dtime: u32,
    /// Owner group ID
    pub gid: u16,
    /// Number of hard links
    pub links_count: u16,
    /// Number of 512-byte blocks allocated
    pub blocks: u32,
    /// File flags
    pub flags: u32,
    /// OS-specific value 1
    pub osd1: u32,
    /// Block pointers: 0-11 direct, 12 indirect, 13 double, 14 triple
    pub block: [u32; 15],
    /// File generation (for NFS)
    pub generation: u32,
    /// File ACL (extended attributes)
    pub file_acl: u32,
    /// Directory ACL / high 32 bits of size (for regular files in rev1)
    pub size_high: u32,
    /// Fragment address (obsolete)
    pub faddr: u32,
    /// OS-specific value 2
    pub osd2: [u8; 12],
}

impl Inode {
    /// Get 64-bit file size.
    pub fn size(&self) -> u64 {
        self.size as u64 | ((self.size_high as u64) << 32)
    }

    /// Check if this inode is a directory.
    pub fn is_dir(&self) -> bool {
        (self.mode & S_IFMT) == S_IFDIR
    }

    /// Check if this inode is a regular file.
    pub fn is_file(&self) -> bool {
        (self.mode & S_IFMT) == S_IFREG
    }

    /// Check if this inode is a symbolic link.
    pub fn is_symlink(&self) -> bool {
        (self.mode & S_IFMT) == S_IFLNK
    }
}

/// Directory entry (on-disk format).
///
/// Variable-length structure within directory data blocks.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DirEntryRaw {
    /// Inode number (0 = deleted entry)
    pub inode: u32,
    /// Record length (distance to next entry)
    pub rec_len: u16,
    /// Name length
    pub name_len: u8,
    /// File type (only valid if feature is enabled)
    pub file_type: u8,
    // Name follows (up to 255 bytes, not null-terminated)
}
