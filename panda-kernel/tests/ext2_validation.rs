//! Tests for ext2 superblock and struct validation.
//!
//! These tests verify that malicious or corrupted ext2 disk images are
//! rejected before they can cause integer overflow, division by zero,
//! or out-of-bounds memory reads.

#![no_std]
#![no_main]

extern crate alloc;

use panda_kernel::vfs::ext2::{
    DirEntryRaw, EXT2_SUPER_MAGIC, Inode, S_IFDIR, S_IFLNK, S_IFREG, Superblock,
};

panda_kernel::test_harness!(
    // Superblock::validate tests
    valid_superblock_passes,
    zero_blocks_count_rejected,
    zero_inodes_count_rejected,
    zero_blocks_per_group_rejected,
    zero_inodes_per_group_rejected,
    log_block_size_too_large_rejected,
    inode_size_too_small_rejected,
    inode_size_too_large_rejected,
    rev0_ignores_inode_size_field,
    excessive_block_groups_rejected,
    // Superblock::block_size tests
    block_size_1k,
    block_size_2k,
    block_size_4k,
    block_size_64k,
    block_size_invalid,
    // Superblock::block_group_count tests
    block_group_count_normal,
    block_group_count_rounds_up,
    block_group_count_zero_bpg,
    // Superblock::inode_size tests
    inode_size_rev0,
    inode_size_rev1,
    inode_size_rev1_256,
    // Superblock feature checks
    unsupported_incompat_detected,
    supported_incompat_passes,
    // Inode helper tests
    inode_size_combines_high_low,
    inode_is_dir,
    inode_is_file,
    inode_is_symlink,
    // DirEntryRaw basic structure
    dir_entry_size_is_8,
    // Edge cases
    max_valid_log_block_size,
    boundary_block_group_count,
    inode_size_boundary_128,
    inode_size_boundary_1024,
);

/// Create a valid superblock for testing. All fields are set to
/// reasonable defaults; individual tests override specific fields.
fn make_valid_superblock() -> Superblock {
    // Safety: zero-init and then set required fields.
    // Superblock is repr(C) and all-zeros is a valid bit pattern.
    let mut sb: Superblock = unsafe { core::mem::zeroed() };
    sb.magic = EXT2_SUPER_MAGIC;
    sb.blocks_count = 8192;
    sb.inodes_count = 2048;
    sb.blocks_per_group = 8192;
    sb.inodes_per_group = 2048;
    sb.log_block_size = 0; // 1024-byte blocks
    sb.rev_level = 1;
    sb.inode_size = 128;
    sb
}

/// Create an inode with zeroed fields for testing.
fn make_inode() -> Inode {
    unsafe { core::mem::zeroed() }
}

// =============================================================================
// Superblock::validate tests
// =============================================================================

fn valid_superblock_passes() {
    let sb = make_valid_superblock();
    assert!(sb.validate().is_ok(), "Valid superblock should pass validation");
}

fn zero_blocks_count_rejected() {
    let mut sb = make_valid_superblock();
    sb.blocks_count = 0;
    assert_eq!(
        sb.validate().unwrap_err(),
        "ext2: blocks_count is zero"
    );
}

fn zero_inodes_count_rejected() {
    let mut sb = make_valid_superblock();
    sb.inodes_count = 0;
    assert_eq!(
        sb.validate().unwrap_err(),
        "ext2: inodes_count is zero"
    );
}

fn zero_blocks_per_group_rejected() {
    let mut sb = make_valid_superblock();
    sb.blocks_per_group = 0;
    assert_eq!(
        sb.validate().unwrap_err(),
        "ext2: blocks_per_group is zero"
    );
}

fn zero_inodes_per_group_rejected() {
    let mut sb = make_valid_superblock();
    sb.inodes_per_group = 0;
    assert_eq!(
        sb.validate().unwrap_err(),
        "ext2: inodes_per_group is zero"
    );
}

fn log_block_size_too_large_rejected() {
    let mut sb = make_valid_superblock();
    sb.log_block_size = 7; // Would be 1024 << 7 = 128KB, beyond max
    assert_eq!(
        sb.validate().unwrap_err(),
        "ext2: log_block_size out of range"
    );
}

fn inode_size_too_small_rejected() {
    let mut sb = make_valid_superblock();
    sb.rev_level = 1;
    sb.inode_size = 64; // Below minimum of 128
    assert_eq!(
        sb.validate().unwrap_err(),
        "ext2: inode_size out of range"
    );
}

fn inode_size_too_large_rejected() {
    let mut sb = make_valid_superblock();
    sb.rev_level = 1;
    sb.inode_size = 2048; // Above maximum of 1024
    assert_eq!(
        sb.validate().unwrap_err(),
        "ext2: inode_size out of range"
    );
}

fn rev0_ignores_inode_size_field() {
    let mut sb = make_valid_superblock();
    sb.rev_level = 0;
    sb.inode_size = 0; // Invalid value, but rev0 ignores it
    assert!(
        sb.validate().is_ok(),
        "Rev0 should ignore inode_size field"
    );
}

fn excessive_block_groups_rejected() {
    let mut sb = make_valid_superblock();
    // blocks_count / blocks_per_group > MAX_BLOCK_GROUPS (1_000_000)
    sb.blocks_count = u32::MAX;
    sb.blocks_per_group = 1; // Would create ~4 billion block groups
    assert_eq!(
        sb.validate().unwrap_err(),
        "ext2: too many block groups"
    );
}

// =============================================================================
// Superblock::block_size tests
// =============================================================================

fn block_size_1k() {
    let mut sb = make_valid_superblock();
    sb.log_block_size = 0;
    assert_eq!(sb.block_size(), Some(1024));
}

fn block_size_2k() {
    let mut sb = make_valid_superblock();
    sb.log_block_size = 1;
    assert_eq!(sb.block_size(), Some(2048));
}

fn block_size_4k() {
    let mut sb = make_valid_superblock();
    sb.log_block_size = 2;
    assert_eq!(sb.block_size(), Some(4096));
}

fn block_size_64k() {
    let mut sb = make_valid_superblock();
    sb.log_block_size = 6; // 1024 << 6 = 65536
    assert_eq!(sb.block_size(), Some(65536));
}

fn block_size_invalid() {
    let mut sb = make_valid_superblock();
    sb.log_block_size = 7;
    assert_eq!(sb.block_size(), None);
}

// =============================================================================
// Superblock::block_group_count tests
// =============================================================================

fn block_group_count_normal() {
    let mut sb = make_valid_superblock();
    sb.blocks_count = 8192;
    sb.blocks_per_group = 8192;
    assert_eq!(sb.block_group_count(), Some(1));
}

fn block_group_count_rounds_up() {
    let mut sb = make_valid_superblock();
    sb.blocks_count = 8193;
    sb.blocks_per_group = 8192;
    assert_eq!(sb.block_group_count(), Some(2));
}

fn block_group_count_zero_bpg() {
    let mut sb = make_valid_superblock();
    sb.blocks_per_group = 0;
    assert_eq!(sb.block_group_count(), None);
}

// =============================================================================
// Superblock::inode_size tests
// =============================================================================

fn inode_size_rev0() {
    let mut sb = make_valid_superblock();
    sb.rev_level = 0;
    sb.inode_size = 999; // Should be ignored
    assert_eq!(sb.inode_size(), 128);
}

fn inode_size_rev1() {
    let mut sb = make_valid_superblock();
    sb.rev_level = 1;
    sb.inode_size = 128;
    assert_eq!(sb.inode_size(), 128);
}

fn inode_size_rev1_256() {
    let mut sb = make_valid_superblock();
    sb.rev_level = 1;
    sb.inode_size = 256;
    assert_eq!(sb.inode_size(), 256);
}

// =============================================================================
// Feature flag tests
// =============================================================================

fn unsupported_incompat_detected() {
    let mut sb = make_valid_superblock();
    sb.feature_incompat = 0x0040; // EXTENTS - not supported
    assert_ne!(sb.unsupported_incompat_features(), 0);
}

fn supported_incompat_passes() {
    let mut sb = make_valid_superblock();
    sb.feature_incompat = 0x0002; // FILETYPE - supported
    assert_eq!(sb.unsupported_incompat_features(), 0);
}

// =============================================================================
// Inode helper tests
// =============================================================================

fn inode_size_combines_high_low() {
    let mut inode = make_inode();
    inode.size = 0x1000;
    inode.size_high = 0x0001;
    assert_eq!(inode.size(), 0x0001_0000_1000);
}

fn inode_is_dir() {
    let mut inode = make_inode();
    inode.mode = S_IFDIR | 0o755;
    assert!(inode.is_dir());
    assert!(!inode.is_file());
    assert!(!inode.is_symlink());
}

fn inode_is_file() {
    let mut inode = make_inode();
    inode.mode = S_IFREG | 0o644;
    assert!(inode.is_file());
    assert!(!inode.is_dir());
    assert!(!inode.is_symlink());
}

fn inode_is_symlink() {
    let mut inode = make_inode();
    inode.mode = S_IFLNK | 0o777;
    assert!(inode.is_symlink());
    assert!(!inode.is_dir());
    assert!(!inode.is_file());
}

// =============================================================================
// DirEntryRaw structure tests
// =============================================================================

fn dir_entry_size_is_8() {
    // The code assumes DirEntryRaw header is 8 bytes (rec_len minimum).
    // Verify the struct layout matches this assumption.
    assert_eq!(core::mem::size_of::<DirEntryRaw>(), 8);
}

// =============================================================================
// Edge case / boundary tests
// =============================================================================

fn max_valid_log_block_size() {
    let mut sb = make_valid_superblock();
    sb.log_block_size = 6; // Maximum valid value
    assert!(sb.validate().is_ok());
    assert_eq!(sb.block_size(), Some(65536));
}

fn boundary_block_group_count() {
    let mut sb = make_valid_superblock();
    // Exactly at the MAX_BLOCK_GROUPS limit (1_000_000)
    sb.blocks_count = 1_000_000;
    sb.blocks_per_group = 1;
    assert!(sb.validate().is_ok(), "Exactly MAX_BLOCK_GROUPS should pass");

    // One over the limit
    sb.blocks_count = 1_000_001;
    sb.blocks_per_group = 1;
    assert!(
        sb.validate().is_err(),
        "One over MAX_BLOCK_GROUPS should fail"
    );
}

fn inode_size_boundary_128() {
    let mut sb = make_valid_superblock();
    sb.rev_level = 1;
    sb.inode_size = 128; // Minimum valid
    assert!(sb.validate().is_ok());
}

fn inode_size_boundary_1024() {
    let mut sb = make_valid_superblock();
    sb.rev_level = 1;
    sb.inode_size = 1024; // Maximum valid
    assert!(sb.validate().is_ok());
}
