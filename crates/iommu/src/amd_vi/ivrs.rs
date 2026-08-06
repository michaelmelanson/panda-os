//! Parser stub for the IVRS ACPI table (AMD-Vi's analogue of DMAR).
//!
//! Not yet implemented; establishes the module so a future implementation
//! doesn't need structural changes elsewhere.

/// Parsed contents of an IVRS table. Currently empty — no fields are populated
/// since parsing is not implemented.
#[derive(Debug, Default)]
pub struct IvrsTable;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IvrsError {
    NotImplemented,
}

pub fn parse(_bytes: &[u8]) -> Result<IvrsTable, IvrsError> {
    Err(IvrsError::NotImplemented)
}
