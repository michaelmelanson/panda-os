//! Parser for the DMAR ACPI table (Intel VT-d specification, chapter 8).
//!
//! Pure byte-slice parsing — no kernel or hardware access. The kernel maps the
//! ACPI table and passes the raw bytes to [`parse`].

use alloc::vec::Vec;

/// A device scope entry: identifies a PCI device (or bridge path) covered by a
/// DRHD or RMRR entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceScope {
    pub scope_type: u8,
    pub start_bus: u8,
    /// (device, function) pairs describing the path from `start_bus` to the device.
    pub path: Vec<(u8, u8)>,
}

/// A DRHD (DMA Remapping Hardware Definition) sub-table: one per IOMMU unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrhEntry {
    pub register_base: u64,
    pub segment: u16,
    /// If true, this unit covers all devices in the segment not explicitly
    /// listed under another DRHD (the `INCLUDE_PCI_ALL` flag).
    pub include_all: bool,
    pub device_scopes: Vec<DeviceScope>,
}

/// An RMRR (Reserved Memory Region Reporting) sub-table: describes a physical
/// memory range firmware requires certain devices to be able to DMA to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RmrrEntry {
    pub base_address: u64,
    pub limit_address: u64,
    pub segment: u16,
    pub device_scopes: Vec<DeviceScope>,
}

/// The parsed contents of a DMAR ACPI table.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DmarTable {
    pub host_address_width: u8,
    pub flags: u8,
    pub drhd: Vec<DrhEntry>,
    pub rmrr: Vec<RmrrEntry>,
}

impl DmarTable {
    pub fn drhd_entries(&self) -> impl Iterator<Item = &DrhEntry> {
        self.drhd.iter()
    }

    pub fn rmrr_entries(&self) -> impl Iterator<Item = &RmrrEntry> {
        self.rmrr.iter()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DmarError {
    TooShort,
    BadSignature,
    TruncatedSubTable,
}

const DMAR_HEADER_LEN: usize = 48;
const SUB_TABLE_HEADER_LEN: usize = 4;
const DEVICE_SCOPE_HEADER_LEN: usize = 6;

const DRHD_TYPE: u16 = 0;
const RMRR_TYPE: u16 = 1;

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(offset..offset + 2)?.try_into().ok()?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(offset..offset + 8)?.try_into().ok()?))
}

/// Parse the device scope list occupying `bytes[..len]`.
fn parse_device_scopes(bytes: &[u8]) -> Result<Vec<DeviceScope>, DmarError> {
    let mut scopes = Vec::new();
    let mut offset = 0;

    while offset < bytes.len() {
        if offset + DEVICE_SCOPE_HEADER_LEN > bytes.len() {
            return Err(DmarError::TruncatedSubTable);
        }
        let scope_type = bytes[offset];
        let scope_len = bytes[offset + 1] as usize;
        let start_bus = bytes[offset + 5];

        if scope_len < DEVICE_SCOPE_HEADER_LEN || offset + scope_len > bytes.len() {
            return Err(DmarError::TruncatedSubTable);
        }

        let path_bytes = &bytes[offset + DEVICE_SCOPE_HEADER_LEN..offset + scope_len];
        let mut path = Vec::new();
        for pair in path_bytes.chunks_exact(2) {
            path.push((pair[0], pair[1]));
        }

        scopes.push(DeviceScope {
            scope_type,
            start_bus,
            path,
        });

        offset += scope_len;
    }

    Ok(scopes)
}

/// Parse the raw bytes of a DMAR ACPI table (including its standard SDT header).
///
/// Layout:
/// - Standard ACPI SDT header (36 bytes): signature, length, revision, checksum, OEM fields
/// - Host address width (1 byte), flags (1 byte), reserved (10 bytes) — total 48 bytes
/// - Sub-tables until `length`: each starts with a 2-byte type and 2-byte length
pub fn parse(bytes: &[u8]) -> Result<DmarTable, DmarError> {
    if bytes.len() < DMAR_HEADER_LEN {
        return Err(DmarError::TooShort);
    }
    if &bytes[0..4] != b"DMAR" {
        return Err(DmarError::BadSignature);
    }

    let total_length = read_u32(bytes, 4).ok_or(DmarError::TooShort)? as usize;
    let total_length = total_length.min(bytes.len());

    let host_address_width = bytes[36];
    let flags = bytes[37];

    let mut table = DmarTable {
        host_address_width,
        flags,
        drhd: Vec::new(),
        rmrr: Vec::new(),
    };

    let mut offset = DMAR_HEADER_LEN;
    while offset + SUB_TABLE_HEADER_LEN <= total_length {
        let sub_type = read_u16(bytes, offset).ok_or(DmarError::TruncatedSubTable)?;
        let sub_len = read_u16(bytes, offset + 2).ok_or(DmarError::TruncatedSubTable)? as usize;

        if sub_len < SUB_TABLE_HEADER_LEN || offset + sub_len > total_length {
            return Err(DmarError::TruncatedSubTable);
        }
        let sub_bytes = &bytes[offset..offset + sub_len];

        match sub_type {
            DRHD_TYPE => {
                // DRHD: type(2), length(2), flags(1), reserved(1), segment(2), reg base(8)
                if sub_bytes.len() < 16 {
                    return Err(DmarError::TruncatedSubTable);
                }
                let drhd_flags = sub_bytes[4];
                let segment = read_u16(sub_bytes, 6).ok_or(DmarError::TruncatedSubTable)?;
                let register_base = read_u64(sub_bytes, 8).ok_or(DmarError::TruncatedSubTable)?;
                let device_scopes = parse_device_scopes(&sub_bytes[16..])?;

                table.drhd.push(DrhEntry {
                    register_base,
                    segment,
                    include_all: drhd_flags & 0x1 != 0,
                    device_scopes,
                });
            }
            RMRR_TYPE => {
                // RMRR: type(2), length(2), reserved(2), segment(2), base(8), limit(8)
                if sub_bytes.len() < 24 {
                    return Err(DmarError::TruncatedSubTable);
                }
                let segment = read_u16(sub_bytes, 6).ok_or(DmarError::TruncatedSubTable)?;
                let base_address = read_u64(sub_bytes, 8).ok_or(DmarError::TruncatedSubTable)?;
                let limit_address = read_u64(sub_bytes, 16).ok_or(DmarError::TruncatedSubTable)?;
                let device_scopes = parse_device_scopes(&sub_bytes[24..])?;

                table.rmrr.push(RmrrEntry {
                    base_address,
                    limit_address,
                    segment,
                    device_scopes,
                });
            }
            _ => {
                // Unknown sub-table type (e.g. ATSR, RHSA, ANDD) — skip.
            }
        }

        offset += sub_len;
    }

    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Build a minimal DMAR header (48 bytes) with the given total length.
    fn header(total_length: u32) -> Vec<u8> {
        let mut bytes = vec![0u8; DMAR_HEADER_LEN];
        bytes[0..4].copy_from_slice(b"DMAR");
        bytes[4..8].copy_from_slice(&total_length.to_le_bytes());
        bytes[36] = 39; // host address width
        bytes[37] = 0; // flags
        bytes
    }

    #[test]
    fn parse_rejects_bad_signature() {
        let mut bytes = header(DMAR_HEADER_LEN as u32);
        bytes[0..4].copy_from_slice(b"XXXX");
        assert_eq!(parse(&bytes), Err(DmarError::BadSignature));
    }

    #[test]
    fn parse_rejects_too_short() {
        assert_eq!(parse(&[0u8; 10]), Err(DmarError::TooShort));
    }

    #[test]
    fn parse_single_drhd_no_device_scopes() {
        let mut bytes = header(0); // patched below

        // DRHD sub-table: type(2)=0, length(2)=16, flags(1)=0, reserved(1)=0,
        // segment(2)=0, register_base(8)=0xFED9_0000
        let mut drhd = vec![0u8; 16];
        drhd[0..2].copy_from_slice(&0u16.to_le_bytes()); // type = DRHD
        drhd[2..4].copy_from_slice(&16u16.to_le_bytes()); // length
        drhd[4] = 0x01; // INCLUDE_PCI_ALL
        drhd[6..8].copy_from_slice(&0u16.to_le_bytes()); // segment
        drhd[8..16].copy_from_slice(&0xFED9_0000u64.to_le_bytes());

        bytes.extend_from_slice(&drhd);
        let total_len = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&total_len.to_le_bytes());

        let table = parse(&bytes).expect("parse failed");
        assert_eq!(table.drhd.len(), 1);
        assert_eq!(table.drhd[0].register_base, 0xFED9_0000);
        assert_eq!(table.drhd[0].segment, 0);
        assert!(table.drhd[0].include_all);
        assert!(table.drhd[0].device_scopes.is_empty());
        assert!(table.rmrr.is_empty());
    }

    #[test]
    fn parse_drhd_with_device_scope() {
        let mut bytes = header(0);

        // Device scope: type(1)=1 (PCI endpoint), length(1)=8, reserved(2),
        // start_bus(1), reserved(1), path: (device, function) = (0x02, 0x00)
        let mut scope = vec![0u8; 8];
        scope[0] = 1; // scope type
        scope[1] = 8; // scope length
        scope[5] = 0x00; // start bus
        scope[6] = 0x02; // device
        scope[7] = 0x00; // function

        let mut drhd = vec![0u8; 16];
        let drhd_len = 16 + scope.len();
        drhd[0..2].copy_from_slice(&0u16.to_le_bytes());
        drhd[2..4].copy_from_slice(&(drhd_len as u16).to_le_bytes());
        drhd[4] = 0x00;
        drhd[6..8].copy_from_slice(&0u16.to_le_bytes());
        drhd[8..16].copy_from_slice(&0xFED9_0000u64.to_le_bytes());
        drhd.extend_from_slice(&scope);

        bytes.extend_from_slice(&drhd);
        let total_len = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&total_len.to_le_bytes());

        let table = parse(&bytes).expect("parse failed");
        assert_eq!(table.drhd.len(), 1);
        assert!(!table.drhd[0].include_all);
        assert_eq!(table.drhd[0].device_scopes.len(), 1);
        let ds = &table.drhd[0].device_scopes[0];
        assert_eq!(ds.start_bus, 0);
        assert_eq!(ds.path, vec![(0x02, 0x00)]);
    }

    #[test]
    fn parse_rmrr_entry() {
        let mut bytes = header(0);

        // RMRR: type(2)=1, length(2)=24, reserved(2), segment(2), base(8), limit(8)
        let mut rmrr = vec![0u8; 24];
        rmrr[0..2].copy_from_slice(&1u16.to_le_bytes());
        rmrr[2..4].copy_from_slice(&24u16.to_le_bytes());
        rmrr[6..8].copy_from_slice(&0u16.to_le_bytes());
        rmrr[8..16].copy_from_slice(&0x000A_0000u64.to_le_bytes());
        rmrr[16..24].copy_from_slice(&0x000B_FFFFu64.to_le_bytes());

        bytes.extend_from_slice(&rmrr);
        let total_len = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&total_len.to_le_bytes());

        let table = parse(&bytes).expect("parse failed");
        assert_eq!(table.rmrr.len(), 1);
        assert_eq!(table.rmrr[0].base_address, 0x000A_0000);
        assert_eq!(table.rmrr[0].limit_address, 0x000B_FFFF);
        assert!(table.rmrr[0].device_scopes.is_empty());
    }

    #[test]
    fn parse_truncated_sub_table_errors() {
        let mut bytes = header(0);
        // Claim a sub-table of length 16 but only provide 8 bytes.
        let mut drhd = vec![0u8; 8];
        drhd[0..2].copy_from_slice(&0u16.to_le_bytes());
        drhd[2..4].copy_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(&drhd);
        let total_len = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&total_len.to_le_bytes());

        assert_eq!(parse(&bytes), Err(DmarError::TruncatedSubTable));
    }
}
