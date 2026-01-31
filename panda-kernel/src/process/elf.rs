//! ELF binary loading.
//!
//! ## Segment Overlap Handling
//!
//! When ELF segments share pages (due to sub-page alignment), we must handle
//! permission conflicts. For example:
//! - Code segment (RX) ends at 0x...90583
//! - Rodata segment (R) starts at 0x...90583
//! - They share page 0x...90000
//!
//! Our approach:
//! 1. Map the first segment with its permissions (RX)
//! 2. Skip remapping the overlapping page for the second segment
//! 3. If the second segment needs more permissive access (e.g., write), upgrade
//!
//! ### Assumptions
//! - Standard segment order: Code (RX) → Rodata (R) → Data (RW)
//! - The linker doesn't create binaries where code and writable data share pages
//!   (this would violate W^X security policy)
//!
//! ### Limitations
//! - If a malformed binary has RX and RW sharing a page, we'll make it RW,
//!   breaking code execution. The linker should prevent this with MAXPAGESIZE.

use alloc::vec::Vec;

use goblin::elf::{
    Elf,
    program_header::{PT_LOAD, pt_to_str},
};
use log::{debug, trace, warn};
use x86_64::VirtAddr;

use crate::memory::{self, Mapping, MemoryMappingOptions, USER_ADDR_MAX};
use crate::process::ProcessError;

/// Validate ELF segment security constraints.
///
/// Ensures that:
/// 1. p_vaddr is within userspace range (< USER_ADDR_MAX)
/// 2. p_vaddr + p_memsz doesn't overflow
/// 3. p_vaddr + p_memsz doesn't extend into kernel space
/// 4. p_offset + p_filesz doesn't overflow
/// 5. p_offset + p_filesz is within actual file bounds
///
/// Returns Ok(segment_end) on success, or Err with a descriptive message on failure.
fn validate_segment_security(
    header: &goblin::elf::ProgramHeader,
    file_size: usize,
) -> Result<u64, ProcessError> {
    // Validate p_vaddr is in userspace
    if header.p_vaddr > USER_ADDR_MAX {
        warn!(
            "ELF security: p_vaddr {:#x} exceeds USER_ADDR_MAX {:#x}",
            header.p_vaddr, USER_ADDR_MAX
        );
        return Err(ProcessError::InvalidElf("ELF segment p_vaddr in kernel space"));
    }

    // Check for p_vaddr + p_memsz overflow and ensure it doesn't extend into kernel space
    let segment_end = header.p_vaddr.checked_add(header.p_memsz)
        .ok_or_else(|| {
            warn!(
                "ELF security: p_vaddr {:#x} + p_memsz {:#x} overflows",
                header.p_vaddr, header.p_memsz
            );
            ProcessError::InvalidElf("ELF segment address + size overflows")
        })?;

    if segment_end > USER_ADDR_MAX {
        warn!(
            "ELF security: segment end {:#x} exceeds USER_ADDR_MAX {:#x}",
            segment_end, USER_ADDR_MAX
        );
        return Err(ProcessError::InvalidElf("ELF segment extends into kernel space"));
    }

    // Check for p_offset + p_filesz overflow
    let file_end = header.p_offset.checked_add(header.p_filesz)
        .ok_or_else(|| {
            warn!(
                "ELF security: p_offset {:#x} + p_filesz {:#x} overflows",
                header.p_offset, header.p_filesz
            );
            ProcessError::InvalidElf("ELF file offset + size overflows")
        })?;

    // Ensure p_offset + p_filesz is within file bounds
    if file_end as usize > file_size {
        warn!(
            "ELF security: file offset {:#x} + filesz {:#x} = {:#x} exceeds file size {:#x}",
            header.p_offset, header.p_filesz, file_end, file_size
        );
        return Err(ProcessError::InvalidElf("ELF segment offset exceeds file size"));
    }

    Ok(segment_end)
}

/// Load an ELF binary into the current address space.
/// Returns the list of mappings created, or an error if the ELF is invalid.
pub fn load_elf(elf: &Elf<'_>, file_ptr: *const [u8]) -> Result<Vec<Mapping>, ProcessError> {
    let mut mappings = Vec::new();
    let mut last_segment_end: u64 = 0;
    let mut last_segment_executable = false;

    // Get the actual ELF file size for offset validation
    let file_size = unsafe { (&(*file_ptr)).len() };

    for header in &elf.program_headers {
        match header.p_type {
            PT_LOAD => {
                // Validate segment security constraints
                validate_segment_security(header, file_size)?;

                let virt_addr = VirtAddr::new(header.p_vaddr);
                let page_offset = virt_addr.as_u64() & 0xFFF;
                let mut aligned_virt_addr = virt_addr.align_down(4096u64);
                let mut aligned_size = header.p_memsz as usize + page_offset as usize;
                let original_aligned_virt = aligned_virt_addr;

                // Handle overlapping segments (segments sharing page boundaries)
                if aligned_virt_addr.as_u64() < last_segment_end {
                    let overlap = last_segment_end - aligned_virt_addr.as_u64();
                    debug!("ELF LOAD: Segment overlap detected: {:#x} bytes", overlap);

                    // Adjust mapping to skip already-mapped pages
                    aligned_virt_addr = VirtAddr::new(last_segment_end);
                    if aligned_size > overlap as usize {
                        aligned_size -= overlap as usize;
                    } else {
                        aligned_size = 0;
                    }

                    // Upgrade permissions if this segment needs write access
                    if header.is_write() {
                        // Safety check: warn if we're about to make executable pages writable
                        if last_segment_executable {
                            warn!(
                                "ELF segment overlap: making executable pages writable (RX+RW at {:#x}). \
                                 This violates W^X policy and may indicate a malformed binary.",
                                original_aligned_virt.as_u64()
                            );
                        }

                        let overlap_size =
                            (aligned_virt_addr.as_u64() - original_aligned_virt.as_u64()) as usize;
                        debug!("ELF LOAD: Upgrading overlapping pages to RW (removing execute)");
                        memory::update_permissions(
                            original_aligned_virt,
                            overlap_size,
                            MemoryMappingOptions {
                                user: true,
                                executable: false, // W^X: can't be both writable and executable
                                writable: true,
                            },
                        );
                    }
                }

                debug!(
                    "ELF LOAD: vaddr={:#x} memsz={:#x} aligned_vaddr={:#x} aligned_size={:#x} flags={}{}{}",
                    header.p_vaddr,
                    header.p_memsz,
                    aligned_virt_addr.as_u64(),
                    aligned_size,
                    if header.is_read() { "R" } else { "" },
                    if header.is_write() { "W" } else { "" },
                    if header.is_executable() { "X" } else { "" },
                );

                if aligned_size > 0 {
                    let mapping = memory::allocate_and_map(
                        aligned_virt_addr,
                        aligned_size,
                        MemoryMappingOptions {
                            user: true,
                            executable: header.is_executable(),
                            writable: header.is_write(),
                        },
                    );

                    // Copy ELF data to the mapped region via the physical window.
                    // The offset within the mapping accounts for page alignment.
                    let src_data = unsafe {
                        core::slice::from_raw_parts(
                            file_ptr.byte_add(header.p_offset as usize) as *const u8,
                            header.p_filesz as usize,
                        )
                    };
                    // Calculate offset: virt_addr may be unaligned, mapping starts at aligned_virt_addr
                    let offset_in_mapping =
                        (virt_addr.as_u64() - aligned_virt_addr.as_u64()) as usize;
                    unsafe {
                        mapping.write_at(offset_in_mapping, src_data);
                    }

                    last_segment_end =
                        aligned_virt_addr.as_u64() + ((aligned_size + 4095) & !4095) as u64;
                    mappings.push(mapping);
                }

                // Track if this segment was executable for overlap detection
                last_segment_executable = header.is_executable();
            }

            _ => trace!("Ignoring {} program header", pt_to_str(header.p_type)),
        }
    }

    Ok(mappings)
}
