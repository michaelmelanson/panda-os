//! Block interface for random-access data (files, disks, memory regions).

use alloc::vec;

/// Errors that can occur during block operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    /// Invalid offset (e.g., seek before start or past end).
    InvalidOffset,
    /// Write operation not supported (read-only resource).
    NotWritable,
    /// Read operation not supported.
    NotReadable,
    /// I/O error during operation.
    IoError,
}

/// Interface for random-access block data.
///
/// Implemented by files, disk partitions, memory regions, etc.
/// Operations are stateless - offsets are provided explicitly.
pub trait Block: Send + Sync {
    /// Read data at the given offset.
    ///
    /// Returns the bytes read. May return fewer bytes than requested if at EOF.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, BlockError>;

    /// Write data at the given offset.
    ///
    /// Returns the number of bytes written.
    fn write_at(&self, _offset: u64, _buf: &[u8]) -> Result<usize, BlockError> {
        Err(BlockError::NotWritable)
    }

    /// Get the size of this block in bytes.
    fn size(&self) -> u64;

    /// Sync any buffered writes to backing storage.
    fn sync(&self) -> Result<(), BlockError> {
        Ok(())
    }
}

/// Low-level block device interface (sector-based).
///
/// Implemented by device drivers (virtio-blk, AHCI, NVMe).
/// Use [`BlockDeviceWrapper`] to get a byte-level [`Block`] interface.
pub trait BlockDevice: Send + Sync {
    /// Read sectors starting at `start_sector` into `buf`.
    ///
    /// `buf.len()` must be a multiple of [`sector_size()`](Self::sector_size).
    fn read_sectors(&self, start_sector: u64, buf: &mut [u8]) -> Result<(), BlockError>;

    /// Write sectors starting at `start_sector` from `buf`.
    ///
    /// `buf.len()` must be a multiple of [`sector_size()`](Self::sector_size).
    fn write_sectors(&self, start_sector: u64, buf: &[u8]) -> Result<(), BlockError>;

    /// Sector size in bytes (typically 512 or 4096).
    fn sector_size(&self) -> u32;

    /// Total number of sectors.
    fn sector_count(&self) -> u64;

    /// Flush any cached writes to storage.
    fn flush(&self) -> Result<(), BlockError> {
        Ok(())
    }
}

/// Wraps a [`BlockDevice`] reference to provide a byte-level [`Block`] interface.
///
/// Handles sector alignment automatically:
/// - Aligned reads/writes pass through directly
/// - Unaligned operations use read-modify-write
pub struct BlockDeviceWrapper<'a, D: ?Sized> {
    device: &'a D,
}

impl<'a, D: BlockDevice + ?Sized> BlockDeviceWrapper<'a, D> {
    /// Create a new wrapper around a block device reference.
    pub fn new(device: &'a D) -> Self {
        Self { device }
    }

    /// Get a reference to the underlying device.
    pub fn device(&self) -> &D {
        self.device
    }
}

impl<D: BlockDevice + ?Sized> Block for BlockDeviceWrapper<'_, D> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, BlockError> {
        if buf.is_empty() {
            return Ok(0);
        }

        let sector_size = self.device.sector_size() as u64;
        let total_size = self.device.sector_count() * sector_size;

        // Check bounds
        if offset >= total_size {
            return Ok(0);
        }

        // Clamp read to device size
        let available = total_size - offset;
        let to_read = (buf.len() as u64).min(available) as usize;

        let start_sector = offset / sector_size;
        let offset_in_sector = (offset % sector_size) as usize;
        let end_offset = offset + to_read as u64;
        let end_sector = (end_offset + sector_size - 1) / sector_size;
        let num_sectors = end_sector - start_sector;

        // Fast path: aligned read
        if offset_in_sector == 0 && to_read % sector_size as usize == 0 {
            self.device
                .read_sectors(start_sector, &mut buf[..to_read])?;
            return Ok(to_read);
        }

        // Slow path: unaligned read - read full sectors into temp buffer
        let mut sector_buf = vec![0u8; (num_sectors * sector_size) as usize];
        self.device.read_sectors(start_sector, &mut sector_buf)?;

        // Copy the requested portion
        buf[..to_read].copy_from_slice(&sector_buf[offset_in_sector..offset_in_sector + to_read]);

        Ok(to_read)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, BlockError> {
        if buf.is_empty() {
            return Ok(0);
        }

        let sector_size = self.device.sector_size() as u64;
        let total_size = self.device.sector_count() * sector_size;

        // Check bounds
        if offset >= total_size {
            return Err(BlockError::InvalidOffset);
        }

        // Clamp write to device size
        let available = total_size - offset;
        let to_write = (buf.len() as u64).min(available) as usize;

        let start_sector = offset / sector_size;
        let offset_in_sector = (offset % sector_size) as usize;
        let end_offset = offset + to_write as u64;
        let end_sector = (end_offset + sector_size - 1) / sector_size;
        let num_sectors = end_sector - start_sector;

        // Fast path: aligned write
        if offset_in_sector == 0 && to_write % sector_size as usize == 0 {
            self.device.write_sectors(start_sector, &buf[..to_write])?;
            return Ok(to_write);
        }

        // Slow path: unaligned write - read-modify-write
        let mut sector_buf = vec![0u8; (num_sectors * sector_size) as usize];

        // Read existing data
        self.device.read_sectors(start_sector, &mut sector_buf)?;

        // Modify with new data
        sector_buf[offset_in_sector..offset_in_sector + to_write].copy_from_slice(&buf[..to_write]);

        // Write back
        self.device.write_sectors(start_sector, &sector_buf)?;

        Ok(to_write)
    }

    fn size(&self) -> u64 {
        self.device.sector_count() * self.device.sector_size() as u64
    }

    fn sync(&self) -> Result<(), BlockError> {
        self.device.flush()
    }
}
