//! Tests for ext2 write primitives and bitmap operations.
//!
//! These tests verify:
//! - Inode, Superblock, and BlockGroupDescriptor serialisation round-trips
//! - Bitmap bit manipulation correctness
//! - Free count consistency after simulated alloc/free cycles

#![no_std]
#![no_main]

extern crate alloc;

use panda_kernel::vfs::ext2::{
    BlockGroupDescriptor, EXT2_SUPER_MAGIC, Inode, S_IFDIR, S_IFREG, S_IFLNK, Superblock,
};

panda_kernel::test_harness!(
    // Inode serialisation round-trip tests
    inode_to_bytes_round_trip,
    inode_to_bytes_preserves_mode,
    inode_to_bytes_preserves_size,
    inode_to_bytes_preserves_block_pointers,
    inode_to_bytes_preserves_timestamps,
    inode_to_bytes_preserves_links_and_flags,
    inode_to_bytes_zeroed,
    // Superblock serialisation round-trip tests
    superblock_to_bytes_round_trip,
    superblock_to_bytes_preserves_magic,
    superblock_to_bytes_preserves_free_counts,
    superblock_to_bytes_preserves_geometry,
    superblock_to_bytes_preserves_features,
    // BlockGroupDescriptor serialisation round-trip tests
    bgd_to_bytes_round_trip,
    bgd_to_bytes_preserves_bitmap_locations,
    bgd_to_bytes_preserves_free_counts,
    bgd_to_bytes_preserves_used_dirs,
    // Bitmap helper tests (exercised via raw byte manipulation)
    bitmap_find_first_clear_empty,
    bitmap_find_first_clear_partial,
    bitmap_find_first_clear_full_byte,
    bitmap_find_first_clear_all_set,
    bitmap_find_first_clear_respects_max,
    bitmap_find_first_clear_mid_byte,
    bitmap_set_clear_round_trip,
    bitmap_set_preserves_neighbours,
    bitmap_clear_preserves_neighbours,
    bitmap_operations_across_bytes,
    bitmap_first_clear_after_many_set,
    // Struct size assertions
    inode_struct_size_is_128,
    superblock_struct_size_is_1024,
    bgd_struct_size_is_32,
    // Serialisation consistency
    inode_file_type_preserved,
    inode_dir_type_preserved,
    inode_symlink_type_preserved,
    // Free count tracking simulation
    free_count_decrement_consistency,
    free_count_increment_consistency,
    free_count_alloc_free_cycle,
);

// =============================================================================
// Helper constructors
// =============================================================================

fn make_valid_superblock() -> Superblock {
    let mut sb: Superblock = unsafe { core::mem::zeroed() };
    sb.magic = EXT2_SUPER_MAGIC;
    sb.blocks_count = 8192;
    sb.inodes_count = 2048;
    sb.free_blocks_count = 4096;
    sb.free_inodes_count = 1024;
    sb.blocks_per_group = 8192;
    sb.inodes_per_group = 2048;
    sb.log_block_size = 0;
    sb.rev_level = 1;
    sb.inode_size = 128;
    sb.first_data_block = 1;
    sb
}

fn make_inode() -> Inode {
    unsafe { core::mem::zeroed() }
}

fn make_bgd() -> BlockGroupDescriptor {
    unsafe { core::mem::zeroed() }
}

/// Deserialise an Inode from bytes (inverse of to_bytes).
fn inode_from_bytes(bytes: &[u8; 128]) -> Inode {
    unsafe { core::ptr::read(bytes.as_ptr() as *const Inode) }
}

/// Deserialise a Superblock from bytes (inverse of to_bytes).
fn superblock_from_bytes(bytes: &[u8; 1024]) -> Superblock {
    unsafe { core::ptr::read(bytes.as_ptr() as *const Superblock) }
}

/// Deserialise a BlockGroupDescriptor from bytes (inverse of to_bytes).
fn bgd_from_bytes(bytes: &[u8; 32]) -> BlockGroupDescriptor {
    unsafe { core::ptr::read(bytes.as_ptr() as *const BlockGroupDescriptor) }
}

// =============================================================================
// Bitmap helper functions (re-implemented here for testing without relying
// on the kernel module's private functions)
// =============================================================================

fn find_first_clear_bit(bitmap: &[u8], max_bits: usize) -> Option<usize> {
    for (byte_idx, &byte) in bitmap.iter().enumerate() {
        if byte == 0xFF {
            continue;
        }
        for bit in 0..8u32 {
            let index = byte_idx * 8 + bit as usize;
            if index >= max_bits {
                return None;
            }
            if byte & (1 << bit) == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn get_bit(bitmap: &[u8], index: usize) -> bool {
    bitmap[index / 8] & (1 << (index % 8)) != 0
}

fn set_bit(bitmap: &mut [u8], index: usize) {
    bitmap[index / 8] |= 1 << (index % 8);
}

fn clear_bit(bitmap: &mut [u8], index: usize) {
    bitmap[index / 8] &= !(1 << (index % 8));
}

// =============================================================================
// Inode serialisation round-trip tests
// =============================================================================

fn inode_to_bytes_round_trip() {
    let mut inode = make_inode();
    inode.mode = S_IFREG | 0o644;
    inode.uid = 1000;
    inode.size = 0x12345678;
    inode.atime = 1700000000;
    inode.ctime = 1700000100;
    inode.mtime = 1700000200;
    inode.gid = 1000;
    inode.links_count = 1;
    inode.blocks = 8;
    inode.flags = 0;
    inode.block[0] = 100;
    inode.block[1] = 101;
    inode.size_high = 0x0001;

    let bytes = inode.to_bytes();
    let restored = inode_from_bytes(&bytes);

    assert_eq!(restored.mode, inode.mode);
    assert_eq!(restored.uid, inode.uid);
    assert_eq!(restored.size, inode.size);
    assert_eq!(restored.atime, inode.atime);
    assert_eq!(restored.ctime, inode.ctime);
    assert_eq!(restored.mtime, inode.mtime);
    assert_eq!(restored.gid, inode.gid);
    assert_eq!(restored.links_count, inode.links_count);
    assert_eq!(restored.blocks, inode.blocks);
    assert_eq!(restored.flags, inode.flags);
    assert_eq!(restored.block[0], inode.block[0]);
    assert_eq!(restored.block[1], inode.block[1]);
    assert_eq!(restored.size_high, inode.size_high);
    assert_eq!(restored.size(), inode.size());
}

fn inode_to_bytes_preserves_mode() {
    let mut inode = make_inode();
    inode.mode = S_IFDIR | 0o755;
    let bytes = inode.to_bytes();
    let restored = inode_from_bytes(&bytes);
    assert_eq!(restored.mode, S_IFDIR | 0o755);
    assert!(restored.is_dir());
}

fn inode_to_bytes_preserves_size() {
    let mut inode = make_inode();
    inode.size = 0xDEADBEEF;
    inode.size_high = 0x0042;
    let bytes = inode.to_bytes();
    let restored = inode_from_bytes(&bytes);
    assert_eq!(restored.size(), 0x0042_DEAD_BEEF);
}

fn inode_to_bytes_preserves_block_pointers() {
    let mut inode = make_inode();
    for i in 0..15 {
        inode.block[i] = (i as u32 + 1) * 100;
    }
    let bytes = inode.to_bytes();
    let restored = inode_from_bytes(&bytes);
    for i in 0..15 {
        assert_eq!(
            restored.block[i],
            (i as u32 + 1) * 100,
            "block[{}] mismatch",
            i
        );
    }
}

fn inode_to_bytes_preserves_timestamps() {
    let mut inode = make_inode();
    inode.atime = 1000;
    inode.ctime = 2000;
    inode.mtime = 3000;
    inode.dtime = 4000;
    let bytes = inode.to_bytes();
    let restored = inode_from_bytes(&bytes);
    assert_eq!(restored.atime, 1000);
    assert_eq!(restored.ctime, 2000);
    assert_eq!(restored.mtime, 3000);
    assert_eq!(restored.dtime, 4000);
}

fn inode_to_bytes_preserves_links_and_flags() {
    let mut inode = make_inode();
    inode.links_count = 42;
    inode.flags = 0x00080000; // EXT4_EXTENTS_FL
    inode.generation = 12345;
    inode.file_acl = 67890;
    let bytes = inode.to_bytes();
    let restored = inode_from_bytes(&bytes);
    assert_eq!(restored.links_count, 42);
    assert_eq!(restored.flags, 0x00080000);
    assert_eq!(restored.generation, 12345);
    assert_eq!(restored.file_acl, 67890);
}

fn inode_to_bytes_zeroed() {
    let inode = make_inode();
    let bytes = inode.to_bytes();
    // All zeros should produce all-zero bytes
    assert!(bytes.iter().all(|&b| b == 0), "Zeroed inode should produce all-zero bytes");
}

// =============================================================================
// Superblock serialisation round-trip tests
// =============================================================================

fn superblock_to_bytes_round_trip() {
    let sb = make_valid_superblock();
    let bytes = sb.to_bytes();
    let restored = superblock_from_bytes(&bytes);

    assert_eq!(restored.magic, EXT2_SUPER_MAGIC);
    assert_eq!(restored.blocks_count, sb.blocks_count);
    assert_eq!(restored.inodes_count, sb.inodes_count);
    assert_eq!(restored.free_blocks_count, sb.free_blocks_count);
    assert_eq!(restored.free_inodes_count, sb.free_inodes_count);
    assert_eq!(restored.blocks_per_group, sb.blocks_per_group);
    assert_eq!(restored.inodes_per_group, sb.inodes_per_group);
    assert_eq!(restored.log_block_size, sb.log_block_size);
    assert_eq!(restored.rev_level, sb.rev_level);
    assert_eq!(restored.inode_size, sb.inode_size);
    assert!(restored.validate().is_ok());
}

fn superblock_to_bytes_preserves_magic() {
    let sb = make_valid_superblock();
    let bytes = sb.to_bytes();
    let restored = superblock_from_bytes(&bytes);
    assert_eq!(restored.magic, 0xEF53);
}

fn superblock_to_bytes_preserves_free_counts() {
    let mut sb = make_valid_superblock();
    sb.free_blocks_count = 1234;
    sb.free_inodes_count = 567;
    let bytes = sb.to_bytes();
    let restored = superblock_from_bytes(&bytes);
    assert_eq!(restored.free_blocks_count, 1234);
    assert_eq!(restored.free_inodes_count, 567);
}

fn superblock_to_bytes_preserves_geometry() {
    let mut sb = make_valid_superblock();
    sb.log_block_size = 2; // 4KB blocks
    sb.blocks_per_group = 32768;
    sb.inodes_per_group = 8192;
    sb.first_data_block = 0;
    let bytes = sb.to_bytes();
    let restored = superblock_from_bytes(&bytes);
    assert_eq!(restored.block_size(), Some(4096));
    assert_eq!(restored.blocks_per_group, 32768);
    assert_eq!(restored.inodes_per_group, 8192);
    assert_eq!(restored.first_data_block, 0);
}

fn superblock_to_bytes_preserves_features() {
    let mut sb = make_valid_superblock();
    sb.feature_compat = 0x0038;
    sb.feature_incompat = 0x0002; // FILETYPE
    sb.feature_ro_compat = 0x007F;
    let bytes = sb.to_bytes();
    let restored = superblock_from_bytes(&bytes);
    assert_eq!(restored.feature_compat, 0x0038);
    assert_eq!(restored.feature_incompat, 0x0002);
    assert_eq!(restored.feature_ro_compat, 0x007F);
}

// =============================================================================
// BlockGroupDescriptor serialisation round-trip tests
// =============================================================================

fn bgd_to_bytes_round_trip() {
    let mut bgd = make_bgd();
    bgd.block_bitmap = 3;
    bgd.inode_bitmap = 4;
    bgd.inode_table = 5;
    bgd.free_blocks_count = 1000;
    bgd.free_inodes_count = 500;
    bgd.used_dirs_count = 10;

    let bytes = bgd.to_bytes();
    let restored = bgd_from_bytes(&bytes);

    assert_eq!(restored.block_bitmap, 3);
    assert_eq!(restored.inode_bitmap, 4);
    assert_eq!(restored.inode_table, 5);
    assert_eq!(restored.free_blocks_count, 1000);
    assert_eq!(restored.free_inodes_count, 500);
    assert_eq!(restored.used_dirs_count, 10);
}

fn bgd_to_bytes_preserves_bitmap_locations() {
    let mut bgd = make_bgd();
    bgd.block_bitmap = 100;
    bgd.inode_bitmap = 101;
    bgd.inode_table = 102;
    let bytes = bgd.to_bytes();
    let restored = bgd_from_bytes(&bytes);
    assert_eq!(restored.block_bitmap, 100);
    assert_eq!(restored.inode_bitmap, 101);
    assert_eq!(restored.inode_table, 102);
}

fn bgd_to_bytes_preserves_free_counts() {
    let mut bgd = make_bgd();
    bgd.free_blocks_count = 0xFFFF;
    bgd.free_inodes_count = 0xFFFF;
    let bytes = bgd.to_bytes();
    let restored = bgd_from_bytes(&bytes);
    assert_eq!(restored.free_blocks_count, 0xFFFF);
    assert_eq!(restored.free_inodes_count, 0xFFFF);
}

fn bgd_to_bytes_preserves_used_dirs() {
    let mut bgd = make_bgd();
    bgd.used_dirs_count = 42;
    let bytes = bgd.to_bytes();
    let restored = bgd_from_bytes(&bytes);
    assert_eq!(restored.used_dirs_count, 42);
}

// =============================================================================
// Bitmap helper tests
// =============================================================================

fn bitmap_find_first_clear_empty() {
    let bitmap = [0x00u8; 128];
    assert_eq!(find_first_clear_bit(&bitmap, 1024), Some(0));
}

fn bitmap_find_first_clear_partial() {
    // First 3 bits set: 0b00000111
    let mut bitmap = [0x00u8; 128];
    bitmap[0] = 0x07;
    assert_eq!(find_first_clear_bit(&bitmap, 1024), Some(3));
}

fn bitmap_find_first_clear_full_byte() {
    let mut bitmap = [0x00u8; 128];
    bitmap[0] = 0xFF;
    assert_eq!(find_first_clear_bit(&bitmap, 1024), Some(8));
}

fn bitmap_find_first_clear_all_set() {
    let bitmap = [0xFFu8; 128];
    assert_eq!(find_first_clear_bit(&bitmap, 1024), None);
}

fn bitmap_find_first_clear_respects_max() {
    // All clear, but max_bits limits search
    let bitmap = [0x00u8; 128];
    assert_eq!(find_first_clear_bit(&bitmap, 0), None);
}

fn bitmap_find_first_clear_mid_byte() {
    // Bits 0,1,2,4 set, bit 3 clear: 0b00010111
    let mut bitmap = [0x00u8; 128];
    bitmap[0] = 0b0001_0111;
    assert_eq!(find_first_clear_bit(&bitmap, 1024), Some(3));
}

fn bitmap_set_clear_round_trip() {
    let mut bitmap = [0x00u8; 128];

    // Set bit 42, verify, clear, verify
    assert!(!get_bit(&bitmap, 42));
    set_bit(&mut bitmap, 42);
    assert!(get_bit(&bitmap, 42));
    clear_bit(&mut bitmap, 42);
    assert!(!get_bit(&bitmap, 42));
}

fn bitmap_set_preserves_neighbours() {
    let mut bitmap = [0x00u8; 128];
    set_bit(&mut bitmap, 5);

    // Bit 5 set, neighbours clear
    assert!(!get_bit(&bitmap, 4));
    assert!(get_bit(&bitmap, 5));
    assert!(!get_bit(&bitmap, 6));

    // Other bytes untouched
    for i in 1..128 {
        assert_eq!(bitmap[i], 0, "byte {} should be untouched", i);
    }
}

fn bitmap_clear_preserves_neighbours() {
    let mut bitmap = [0xFFu8; 128];
    clear_bit(&mut bitmap, 13);

    // Bit 13 clear, neighbours still set
    assert!(get_bit(&bitmap, 12));
    assert!(!get_bit(&bitmap, 13));
    assert!(get_bit(&bitmap, 14));

    // Only byte 1 affected (13/8 = 1)
    assert_eq!(bitmap[0], 0xFF);
    assert_eq!(bitmap[1], 0b1101_1111);
    assert_eq!(bitmap[2], 0xFF);
}

fn bitmap_operations_across_bytes() {
    let mut bitmap = [0x00u8; 128];

    // Set one bit in each of the first 4 bytes
    set_bit(&mut bitmap, 0);
    set_bit(&mut bitmap, 8);
    set_bit(&mut bitmap, 16);
    set_bit(&mut bitmap, 24);

    assert_eq!(bitmap[0], 0x01);
    assert_eq!(bitmap[1], 0x01);
    assert_eq!(bitmap[2], 0x01);
    assert_eq!(bitmap[3], 0x01);

    // Clear them
    clear_bit(&mut bitmap, 0);
    clear_bit(&mut bitmap, 8);
    clear_bit(&mut bitmap, 16);
    clear_bit(&mut bitmap, 24);

    for i in 0..4 {
        assert_eq!(bitmap[i], 0x00, "byte {} should be cleared", i);
    }
}

fn bitmap_first_clear_after_many_set() {
    let mut bitmap = [0x00u8; 128];
    // Set the first 100 bits
    for i in 0..100 {
        set_bit(&mut bitmap, i);
    }
    // First clear should be bit 100
    assert_eq!(find_first_clear_bit(&bitmap, 1024), Some(100));
}

// =============================================================================
// Struct size assertions
// =============================================================================

fn inode_struct_size_is_128() {
    assert_eq!(core::mem::size_of::<Inode>(), 128);
}

fn superblock_struct_size_is_1024() {
    assert_eq!(core::mem::size_of::<Superblock>(), 1024);
}

fn bgd_struct_size_is_32() {
    assert_eq!(core::mem::size_of::<BlockGroupDescriptor>(), 32);
}

// =============================================================================
// Serialisation consistency with type helpers
// =============================================================================

fn inode_file_type_preserved() {
    let mut inode = make_inode();
    inode.mode = S_IFREG | 0o644;
    let bytes = inode.to_bytes();
    let restored = inode_from_bytes(&bytes);
    assert!(restored.is_file());
    assert!(!restored.is_dir());
    assert!(!restored.is_symlink());
}

fn inode_dir_type_preserved() {
    let mut inode = make_inode();
    inode.mode = S_IFDIR | 0o755;
    let bytes = inode.to_bytes();
    let restored = inode_from_bytes(&bytes);
    assert!(restored.is_dir());
    assert!(!restored.is_file());
    assert!(!restored.is_symlink());
}

fn inode_symlink_type_preserved() {
    let mut inode = make_inode();
    inode.mode = S_IFLNK | 0o777;
    let bytes = inode.to_bytes();
    let restored = inode_from_bytes(&bytes);
    assert!(restored.is_symlink());
    assert!(!restored.is_file());
    assert!(!restored.is_dir());
}

// =============================================================================
// Free count tracking simulation
// =============================================================================

/// Simulate the decrement that alloc_block performs on free counts.
fn free_count_decrement_consistency() {
    let mut sb = make_valid_superblock();
    let mut bgd = make_bgd();
    bgd.free_blocks_count = 100;
    sb.free_blocks_count = 1000;

    // Simulate allocation
    bgd.free_blocks_count -= 1;
    sb.free_blocks_count -= 1;

    assert_eq!(bgd.free_blocks_count, 99);
    assert_eq!(sb.free_blocks_count, 999);

    // Verify these survive serialisation
    let sb_bytes = sb.to_bytes();
    let bgd_bytes = bgd.to_bytes();
    let sb_restored = superblock_from_bytes(&sb_bytes);
    let bgd_restored = bgd_from_bytes(&bgd_bytes);
    assert_eq!(sb_restored.free_blocks_count, 999);
    assert_eq!(bgd_restored.free_blocks_count, 99);
}

/// Simulate the increment that free_block performs on free counts.
fn free_count_increment_consistency() {
    let mut sb = make_valid_superblock();
    let mut bgd = make_bgd();
    bgd.free_blocks_count = 99;
    sb.free_blocks_count = 999;

    // Simulate deallocation
    bgd.free_blocks_count += 1;
    sb.free_blocks_count += 1;

    assert_eq!(bgd.free_blocks_count, 100);
    assert_eq!(sb.free_blocks_count, 1000);

    let sb_bytes = sb.to_bytes();
    let bgd_bytes = bgd.to_bytes();
    let sb_restored = superblock_from_bytes(&sb_bytes);
    let bgd_restored = bgd_from_bytes(&bgd_bytes);
    assert_eq!(sb_restored.free_blocks_count, 1000);
    assert_eq!(bgd_restored.free_blocks_count, 100);
}

/// Simulate a full alloc/free cycle: allocate N blocks, free them all,
/// verify counts return to original values.
fn free_count_alloc_free_cycle() {
    let mut sb = make_valid_superblock();
    let mut bgd = make_bgd();
    bgd.free_blocks_count = 50;
    bgd.free_inodes_count = 25;
    sb.free_blocks_count = 50;
    sb.free_inodes_count = 25;

    let original_free_blocks = sb.free_blocks_count;
    let original_free_inodes = sb.free_inodes_count;
    let original_bgd_free_blocks = bgd.free_blocks_count;
    let original_bgd_free_inodes = bgd.free_inodes_count;

    // Simulate allocating 10 blocks and 5 inodes
    let alloc_blocks = 10u32;
    let alloc_inodes = 5u32;

    for _ in 0..alloc_blocks {
        bgd.free_blocks_count -= 1;
        sb.free_blocks_count -= 1;
    }
    for _ in 0..alloc_inodes {
        bgd.free_inodes_count -= 1;
        sb.free_inodes_count -= 1;
    }

    assert_eq!(sb.free_blocks_count, original_free_blocks - alloc_blocks);
    assert_eq!(sb.free_inodes_count, original_free_inodes - alloc_inodes);
    assert_eq!(bgd.free_blocks_count, original_bgd_free_blocks - alloc_blocks as u16);
    assert_eq!(bgd.free_inodes_count, original_bgd_free_inodes - alloc_inodes as u16);

    // Simulate freeing them all
    for _ in 0..alloc_blocks {
        bgd.free_blocks_count += 1;
        sb.free_blocks_count += 1;
    }
    for _ in 0..alloc_inodes {
        bgd.free_inodes_count += 1;
        sb.free_inodes_count += 1;
    }

    assert_eq!(sb.free_blocks_count, original_free_blocks);
    assert_eq!(sb.free_inodes_count, original_free_inodes);
    assert_eq!(bgd.free_blocks_count, original_bgd_free_blocks);
    assert_eq!(bgd.free_inodes_count, original_bgd_free_inodes);

    // Verify serialisation preserves the restored counts
    let sb_bytes = sb.to_bytes();
    let bgd_bytes = bgd.to_bytes();
    let sb_restored = superblock_from_bytes(&sb_bytes);
    let bgd_restored = bgd_from_bytes(&bgd_bytes);
    assert_eq!(sb_restored.free_blocks_count, original_free_blocks);
    assert_eq!(sb_restored.free_inodes_count, original_free_inodes);
    assert_eq!(bgd_restored.free_blocks_count, original_bgd_free_blocks);
    assert_eq!(bgd_restored.free_inodes_count, original_bgd_free_inodes);
}
