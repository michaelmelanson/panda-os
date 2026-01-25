//! Integration tests for block device layer.
//!
//! These tests require QEMU to be run with a virtio-blk disk attached.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use panda_kernel::devices::virtio_block;
use panda_kernel::resource::BlockDevice;

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

/// A no-op waker for busy-polling.
fn noop_waker() -> Waker {
    fn noop_clone(_: *const ()) -> RawWaker {
        RawWaker::new(core::ptr::null(), &NOOP_VTABLE)
    }
    fn noop(_: *const ()) {}

    static NOOP_VTABLE: RawWakerVTable = RawWakerVTable::new(noop_clone, noop, noop, noop);

    unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &NOOP_VTABLE)) }
}

/// Block on a future by busy-polling until it completes.
/// This also polls the virtio block devices to process completions.
fn block_on<T>(mut future: Pin<Box<dyn Future<Output = T>>>) -> T {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);

    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(result) => return result,
            Poll::Pending => {
                // Poll virtio devices to process any completed I/O
                virtio_block::poll_all();
            }
        }
    }
}

/// Get the first block device for testing.
fn get_test_device() -> virtio_block::VirtioBlockDevice {
    let devices = virtio_block::list_devices();
    assert!(
        !devices.is_empty(),
        "No block devices found - is QEMU running with -drive?"
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
    let size = device.size();
    let sector_size = device.sector_size();

    assert!(
        sector_size >= 512,
        "Sector size should be at least 512 bytes"
    );
    assert!(size > 0, "Device should have non-zero size");
    assert!(
        size >= sector_size as u64,
        "Device size should be at least one sector"
    );
}

fn read_sector_aligned() {
    let device = get_test_device();
    let sector_size = device.sector_size() as usize;

    // Write known data first (to sector 10 to avoid other test data)
    let offset = (sector_size * 10) as u64;
    let write_buf = vec![0x42u8; sector_size]; // Fill with 'B'

    let write_result = block_on(Box::pin(async move {
        device.write_at(offset, &write_buf).await
    }));
    assert!(write_result.is_ok(), "Write should succeed");

    // Read it back
    let device2 = get_test_device();
    let mut read_buf = vec![0u8; sector_size];

    let (result, read_buf) = block_on(Box::pin(async move {
        let r = device2.read_at(offset, &mut read_buf).await;
        (r, read_buf)
    }));

    assert!(result.is_ok(), "Sector-aligned read should succeed");
    assert_eq!(result.unwrap(), sector_size, "Should read full sector");
    assert!(
        read_buf.iter().all(|&b| b == 0x42),
        "Should read back written data"
    );
}

fn write_sector_aligned() {
    let device = get_test_device();
    let sector_size = device.sector_size() as usize;

    // Write to sector 1 to avoid overwriting important data
    let offset = sector_size as u64;
    let mut write_buf = vec![0xABu8; sector_size];
    write_buf[0] = 0x50; // 'P'
    write_buf[1] = 0x41; // 'A'
    write_buf[2] = 0x4E; // 'N'
    write_buf[3] = 0x44; // 'D'
    write_buf[4] = 0x41; // 'A'

    let write_result = block_on(Box::pin(async move {
        device.write_at(offset, &write_buf).await
    }));
    assert!(write_result.is_ok(), "Write should succeed");
    assert_eq!(
        write_result.unwrap(),
        sector_size,
        "Should write full sector"
    );

    // Read it back with a fresh device handle
    let device2 = get_test_device();
    let mut read_buf = vec![0u8; sector_size];
    let (read_result, read_buf) = block_on(Box::pin(async move {
        let r = device2.read_at(offset, &mut read_buf).await;
        (r, read_buf)
    }));
    assert!(read_result.is_ok(), "Read should succeed");
    assert_eq!(read_result.unwrap(), sector_size, "Should read full sector");

    // Verify the data we wrote
    assert_eq!(&read_buf[0..5], b"PANDA", "Should read back written data");
    assert_eq!(read_buf[5], 0xAB, "Rest of sector should match");
}

fn read_unaligned_offset() {
    let device = get_test_device();
    let sector_size = device.sector_size() as usize;

    // Write a full sector of known data first (sector 20)
    let sector_offset = (sector_size * 20) as u64;
    let write_buf = vec![0x55u8; sector_size]; // Fill with 'U'

    let write_result = block_on(Box::pin(async move {
        device.write_at(sector_offset, &write_buf).await
    }));
    assert!(write_result.is_ok(), "Write should succeed");

    // Read 100 bytes starting at offset 50 within that sector (not sector-aligned)
    let device2 = get_test_device();
    let read_offset = sector_offset + 50;
    let mut buf = vec![0u8; 100];

    let (result, buf) = block_on(Box::pin(async move {
        let r = device2.read_at(read_offset, &mut buf).await;
        (r, buf)
    }));

    assert!(result.is_ok(), "Unaligned offset read should succeed");
    assert_eq!(result.unwrap(), 100, "Should read requested bytes");
    assert!(
        buf.iter().all(|&b| b == 0x55),
        "Should read back written data"
    );
}

fn read_unaligned_size() {
    let device = get_test_device();
    let sector_size = device.sector_size() as usize;

    // Write a full sector of known data first (sector 30)
    let offset = (sector_size * 30) as u64;
    let write_buf = vec![0x77u8; sector_size]; // Fill with 'w'

    let write_result = block_on(Box::pin(async move {
        device.write_at(offset, &write_buf).await
    }));
    assert!(write_result.is_ok(), "Write should succeed");

    // Read 100 bytes (not a multiple of sector size) from that sector
    let device2 = get_test_device();
    let mut buf = vec![0u8; 100];

    let (result, buf) = block_on(Box::pin(async move {
        let r = device2.read_at(offset, &mut buf).await;
        (r, buf)
    }));

    assert!(result.is_ok(), "Unaligned size read should succeed");
    assert_eq!(result.unwrap(), 100, "Should read requested bytes");
    assert!(
        buf.iter().all(|&b| b == 0x77),
        "Should read back written data"
    );
}

fn write_read_unaligned() {
    let device = get_test_device();
    let sector_size = device.sector_size() as usize;

    // Write 23 bytes at offset 2048 + 50 (unaligned)
    let offset = (sector_size * 4) as u64 + 50;
    let write_data: &[u8] = b"Hello, unaligned world!";
    let write_len = write_data.len();

    let write_result = block_on(Box::pin(async move {
        device.write_at(offset, write_data).await
    }));
    assert!(write_result.is_ok(), "Unaligned write should succeed");
    assert_eq!(write_result.unwrap(), write_len, "Should write all bytes");

    // Read it back
    let device2 = get_test_device();
    let mut read_buf = vec![0u8; write_len];
    let (read_result, read_buf) = block_on(Box::pin(async move {
        let r = device2.read_at(offset, &mut read_buf).await;
        (r, read_buf)
    }));
    assert!(read_result.is_ok(), "Unaligned read should succeed");
    assert_eq!(read_result.unwrap(), write_len, "Should read all bytes");

    // Verify the data we wrote
    assert_eq!(
        &read_buf[..],
        b"Hello, unaligned world!",
        "Should read back written data"
    );
}

fn read_past_eof_returns_zero() {
    let device = get_test_device();
    let size = device.size();

    // Try to read past end of device
    let mut buf = vec![0u8; 512];
    let (result, _buf) = block_on(Box::pin(async move {
        let r = device.read_at(size, &mut buf).await;
        (r, buf)
    }));

    assert!(result.is_ok(), "Read at EOF should succeed");
    assert_eq!(result.unwrap(), 0, "Read at EOF should return 0 bytes");
}
