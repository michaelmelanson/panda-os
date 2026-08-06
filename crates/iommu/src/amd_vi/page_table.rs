//! AMD-Vi page table format — not yet implemented.
//!
//! Key differences from VT-d's second-level page tables, to keep in mind when
//! this is filled in:
//! - AMD-Vi uses a flat device table indexed by device ID to look up each
//!   device's I/O page table root, rather than a root/context table hierarchy.
//! - The page table entry flag bits differ from VT-d's (bit 0 = read,
//!   bit 1 = write on VT-d; AMD-Vi's layout is different and includes extra
//!   permission and mode fields).
//! - Invalidation is performed by submitting commands to an IOMMU command
//!   queue in memory, not by writing invalidation registers directly.
