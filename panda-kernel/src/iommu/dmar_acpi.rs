//! Access to the DMAR ACPI table.
//!
//! Maps the table via the kernel's existing ACPI handler, extracts the raw
//! bytes, and hands them to the crate's hardware-agnostic byte-slice parser.

use acpi::AcpiTable;
use acpi::sdt::{SdtHeader, Signature};
use iommu::vtd::dmar::{DmarError, DmarTable};

/// A minimal typed view of the DMAR table used only to get `acpi::AcpiTables`
/// to map the whole table (by signature and length) so we can read it as raw
/// bytes. All actual field interpretation happens in `iommu::vtd::dmar::parse`.
#[repr(C)]
struct RawDmar {
    header: SdtHeader,
}

unsafe impl AcpiTable for RawDmar {
    const SIGNATURE: Signature = Signature::DMAR;

    fn header(&self) -> &SdtHeader {
        &self.header
    }
}

#[derive(Debug)]
pub enum DmarReadError {
    NotPresent,
    Parse(DmarError),
}

/// Read and parse the DMAR ACPI table, if present.
pub fn read_dmar() -> Result<DmarTable, DmarReadError> {
    let result = core::cell::RefCell::new(Err(DmarReadError::NotPresent));

    crate::acpi::with_table::<RawDmar>(|table| {
        if let Some(table) = table {
            let length = table.header.length as usize;
            let ptr = &*table as *const RawDmar as *const u8;
            // Safety: `find_table` maps exactly `header.length` bytes starting
            // at the table's physical address, so reading `length` bytes from
            // the mapped pointer is in-bounds.
            let bytes = unsafe { core::slice::from_raw_parts(ptr, length) };
            *result.borrow_mut() = iommu::vtd::dmar::parse(bytes).map_err(DmarReadError::Parse);
        }
    });

    result.into_inner()
}
