//! Ext2 on-disk structures.
//!
//! These structures match the ext2 specification for little-endian systems.

/// Ext2 magic number in the superblock.
pub const EXT2_SUPER_MAGIC: u16 = 0xEF53;

/// Root directory inode number (always 2 in ext2).
pub const EXT2_ROOT_INO: u32 = 2;

/// Superblock offset from start of device (in bytes).
pub const SUPERBLOCK_OFFSET: u64 = 1024;

// =============================================================================
// Feature flags
// =============================================================================

// Compatible features (can mount read-write even if not supported)
/// Directory preallocation
pub const COMPAT_DIR_PREALLOC: u32 = 0x0001;
/// "imagic inodes" (AFS server inodes)
pub const COMPAT_IMAGIC_INODES: u32 = 0x0002;
/// Has a journal (ext3)
pub const COMPAT_HAS_JOURNAL: u32 = 0x0004;
/// Extended attributes
pub const COMPAT_EXT_ATTR: u32 = 0x0008;
/// Filesystem can resize itself for larger partitions
pub const COMPAT_RESIZE_INO: u32 = 0x0010;
/// Directories use hash index
pub const COMPAT_DIR_INDEX: u32 = 0x0020;

// Incompatible features (must not mount if not supported)
/// Compression
pub const INCOMPAT_COMPRESSION: u32 = 0x0001;
/// Directory entries have file type byte
pub const INCOMPAT_FILETYPE: u32 = 0x0002;
/// Filesystem needs recovery (journal replay)
pub const INCOMPAT_RECOVER: u32 = 0x0004;
/// Filesystem has separate journal device
pub const INCOMPAT_JOURNAL_DEV: u32 = 0x0008;
/// Meta block groups
pub const INCOMPAT_META_BG: u32 = 0x0010;
/// Files use extents (ext4)
pub const INCOMPAT_EXTENTS: u32 = 0x0040;
/// 64-bit filesystem
pub const INCOMPAT_64BIT: u32 = 0x0080;
/// Multiple mount protection
pub const INCOMPAT_MMP: u32 = 0x0100;
/// Flexible block groups
pub const INCOMPAT_FLEX_BG: u32 = 0x0200;

// Read-only compatible features (can mount read-only if not supported)
/// Sparse superblocks
pub const RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
/// Large files (> 2GB)
pub const RO_COMPAT_LARGE_FILE: u32 = 0x0002;
/// Files use B-tree directories
pub const RO_COMPAT_BTREE_DIR: u32 = 0x0004;
/// Files use huge files
pub const RO_COMPAT_HUGE_FILE: u32 = 0x0008;
/// Group descriptors have checksums
pub const RO_COMPAT_GDT_CSUM: u32 = 0x0010;
/// Large directories (> 32000 subdirs)
pub const RO_COMPAT_DIR_NLINK: u32 = 0x0020;
/// Extra inode size
pub const RO_COMPAT_EXTRA_ISIZE: u32 = 0x0040;

/// Features we support for incompatible feature mask.
/// FILETYPE is common in modern ext2 filesystems.
pub const SUPPORTED_INCOMPAT: u32 = INCOMPAT_FILETYPE;

/// Features we support for read-only compatible feature mask.
/// We support all standard RO_COMPAT features since we're read-only anyway.
pub const SUPPORTED_RO_COMPAT: u32 = RO_COMPAT_SPARSE_SUPER
    | RO_COMPAT_LARGE_FILE
    | RO_COMPAT_BTREE_DIR
    | RO_COMPAT_HUGE_FILE
    | RO_COMPAT_GDT_CSUM
    | RO_COMPAT_DIR_NLINK
    | RO_COMPAT_EXTRA_ISIZE;

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
    /// Maximum valid log_block_size (6 = 64KB blocks).
    const MAX_LOG_BLOCK_SIZE: u32 = 6;

    /// Maximum reasonable block group count to prevent excessive allocations.
    const MAX_BLOCK_GROUPS: u32 = 1_000_000;

    /// Calculate block size from log_block_size.
    ///
    /// Returns `None` if `log_block_size` is out of the valid range [0, 6].
    pub fn block_size(&self) -> Option<u32> {
        if self.log_block_size > Self::MAX_LOG_BLOCK_SIZE {
            return None;
        }
        Some(1024 << self.log_block_size)
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
    ///
    /// Returns `None` if `blocks_per_group` is zero.
    pub fn block_group_count(&self) -> Option<u32> {
        if self.blocks_per_group == 0 {
            return None;
        }
        Some(self.blocks_count.div_ceil(self.blocks_per_group))
    }

    /// Validate superblock fields for safety.
    ///
    /// Returns `Ok(())` if all critical fields are valid, or an error message.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.log_block_size > Self::MAX_LOG_BLOCK_SIZE {
            log::warn!(
                "ext2: invalid log_block_size {} (max {})",
                self.log_block_size,
                Self::MAX_LOG_BLOCK_SIZE
            );
            return Err("ext2: log_block_size out of range");
        }

        if self.blocks_count == 0 {
            return Err("ext2: blocks_count is zero");
        }

        if self.inodes_count == 0 {
            return Err("ext2: inodes_count is zero");
        }

        if self.blocks_per_group == 0 {
            return Err("ext2: blocks_per_group is zero");
        }

        if self.inodes_per_group == 0 {
            return Err("ext2: inodes_per_group is zero");
        }

        // Validate inode size for rev1+ filesystems
        if self.rev_level >= 1 && (self.inode_size < 128 || self.inode_size > 1024) {
            log::warn!("ext2: invalid inode_size {}", self.inode_size);
            return Err("ext2: inode_size out of range");
        }

        // Validate block group count isn't excessive
        let bg_count = self.blocks_count.div_ceil(self.blocks_per_group);
        if bg_count > Self::MAX_BLOCK_GROUPS {
            log::warn!("ext2: excessive block group count {}", bg_count);
            return Err("ext2: too many block groups");
        }

        Ok(())
    }

    /// Check if the filesystem has unsupported incompatible features.
    ///
    /// Returns the mask of unsupported features, or 0 if all are supported.
    /// For read-only mounts, only INCOMPAT features matter.
    /// For read-write mounts, RO_COMPAT features would also need checking.
    pub fn unsupported_incompat_features(&self) -> u32 {
        self.feature_incompat & !SUPPORTED_INCOMPAT
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
