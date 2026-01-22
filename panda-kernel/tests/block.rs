//! Integration tests for block device layer.
//!
//! These tests require QEMU to be run with a virtio-blk disk attached.

#![no_std]
#![no_main]

extern crate alloc;

use panda_kernel::devices::virtio_block;
use panda_kernel::resource::{Block, BlockDevice, BlockDeviceWrapper};

panda_kernel::test_harness!(
    device_detected,
    device_has_valid_size,
    read_sector_aligned,
    write_sector_aligned,
    read_unaligned_offset,
    read_unaligned_size,
    write_read_unaligned,
    read_past_eof_returns_zero,
);

/// Get the first block device for testing.
fn get_test_device() -> alloc::sync::Arc<spinning_top::Spinlock<virtio_block::VirtioBlockDevice>> {
    let devices = virtio_block::list_devices();
    assert!(
        !devices.is_empty(),
        "No block devices found - is QEMU running with -drive if=virtio?"
    );
    virtio_block::get_device(&devices[0]).expect("Failed to get block device")
}

fn device_detected() {
    let devices = virtio_block::list_devices();
    assert!(
        !devices.is_empty(),
        "Expected at least one virtio-blk device"
    );
}

fn device_has_valid_size() {
    let device = get_test_device();
    let sector_size = device.sector_size();
    let sector_count = device.sector_count();

    assert!(
        sector_size >= 512,
        "Sector size should be at least 512 bytes"
    );
    assert!(sector_count > 0, "Device should have at least one sector");

    let total_size = sector_count * sector_size as u64;
    assert!(total_size > 0, "Total device size should be positive");
}

fn read_sector_aligned() {
    let device = get_test_device();
    let sector_size = device.sector_size() as usize;

    // Read one sector at offset 0
    let mut buf = alloc::vec![0u8; sector_size];
    device
        .read_sectors(0, &mut buf)
        .expect("Aligned sector read failed");

    // Read multiple sectors
    let mut buf = alloc::vec![0u8; sector_size * 2];
    device
        .read_sectors(0, &mut buf)
        .expect("Multi-sector read failed");
}

fn write_sector_aligned() {
    let device = get_test_device();
    let sector_size = device.sector_size() as usize;

    // Write a test pattern to sector 1 (avoid sector 0 which might have special data)
    let mut write_buf = alloc::vec![0xABu8; sector_size];
    write_buf[0] = 0x50; // 'P'
    write_buf[1] = 0x41; // 'A'
    write_buf[2] = 0x4E; // 'N'
    write_buf[3] = 0x44; // 'D'
    write_buf[4] = 0x41; // 'A'

    device
        .write_sectors(1, &write_buf)
        .expect("Aligned sector write failed");

    // Read it back and verify
    let mut read_buf = alloc::vec![0u8; sector_size];
    device
        .read_sectors(1, &mut read_buf)
        .expect("Read after write failed");

    assert_eq!(&read_buf[0..5], b"PANDA", "Written data should match");
    assert_eq!(read_buf[5], 0xAB, "Rest of sector should match");
}

fn read_unaligned_offset() {
    let device = get_test_device();
    let wrapper = BlockDeviceWrapper::new(&*device);

    // First write known data to sector 0
    let sector_size = device.sector_size() as usize;
    let mut write_buf = alloc::vec![0u8; sector_size];
    for i in 0..sector_size {
        write_buf[i] = (i & 0xFF) as u8;
    }
    device
        .write_sectors(0, &write_buf)
        .expect("Setup write failed");

    // Read from unaligned offset (e.g., offset 100)
    let mut buf = [0u8; 50];
    let n = wrapper
        .read_at(100, &mut buf)
        .expect("Unaligned read failed");

    assert_eq!(n, 50, "Should read requested bytes");
    // Verify data matches what we wrote
    for i in 0..50 {
        assert_eq!(
            buf[i],
            ((100 + i) & 0xFF) as u8,
            "Data mismatch at offset {}",
            i
        );
    }
}

fn read_unaligned_size() {
    let device = get_test_device();
    let wrapper = BlockDeviceWrapper::new(&*device);

    // Read 100 bytes (not a multiple of sector size) from offset 0
    let mut buf = [0u8; 100];
    let n = wrapper
        .read_at(0, &mut buf)
        .expect("Unaligned size read failed");

    assert_eq!(n, 100, "Should read requested bytes");
}

fn write_read_unaligned() {
    let device = get_test_device();
    let wrapper = BlockDeviceWrapper::new(&*device);

    // Step 1: Write initial pattern at offset 50 (unaligned)
    let initial_data = [0xAAu8; 64];
    let n = wrapper
        .write_at(50, &initial_data)
        .expect("Initial unaligned write failed");
    assert_eq!(n, 64, "Should write all initial bytes");

    // Step 2: Overwrite first 32 bytes with different pattern
    let overwrite_data = b"OVERWRITE_TEST_1234567890_DONE!";
    assert_eq!(overwrite_data.len(), 31);
    let n = wrapper
        .write_at(50, overwrite_data)
        .expect("Overwrite failed");
    assert_eq!(n, 31, "Should write overwrite bytes");

    // Step 3: Read back and verify against expected final result
    let mut read_buf = [0u8; 64];
    let n = wrapper
        .read_at(50, &mut read_buf)
        .expect("Read after unaligned write failed");
    assert_eq!(n, 64, "Should read all bytes");

    // Expected: 31 bytes of overwrite data + 33 bytes of 0xAA
    let expected: [u8; 64] = *b"OVERWRITE_TEST_1234567890_DONE!\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA";

    assert_eq!(
        read_buf, expected,
        "Data should match expected final result"
    );
}

fn read_past_eof_returns_zero() {
    let device = get_test_device();
    let wrapper = BlockDeviceWrapper::new(&*device);

    let size = wrapper.size();

    // Read starting past EOF
    let mut buf = [0u8; 100];
    let n = wrapper
        .read_at(size + 1000, &mut buf)
        .expect("Read past EOF should succeed");
    assert_eq!(n, 0, "Reading past EOF should return 0 bytes");

    // Read spanning EOF
    let mut buf = [0u8; 100];
    let n = wrapper
        .read_at(size - 50, &mut buf)
        .expect("Read spanning EOF should succeed");
    assert_eq!(n, 50, "Should only read bytes before EOF");
}
