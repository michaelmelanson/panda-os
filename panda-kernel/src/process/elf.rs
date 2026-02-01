//! ELF binary loading with minimal parsing.
//!
//! Uses a custom minimal ELF parser that reads only the ELF header and program
//! headers, skipping section headers, symbol tables, and relocations. This is
//! significantly faster than a full ELF parse (e.g., goblin), especially in
//! debug mode.
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

use log::{debug, trace, warn};
use x86_64::VirtAddr;

use crate::memory::{self, Mapping, MemoryMappingOptions, USER_ADDR_MAX};
use crate::process::ProcessError;

// ELF constants — only the subset we actually need.
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

/// Minimal ELF64 header — only the fields we use.
#[derive(Debug)]
pub struct Elf64Header {
    pub entry: u64,
    pub phoff: u64,
    pub phentsize: u16,
    pub phnum: u16,
}

/// Minimal ELF64 program header — only the fields we use.
#[derive(Debug)]
pub struct Elf64Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
}

impl Elf64Phdr {
    pub fn is_read(&self) -> bool {
        self.p_flags & PF_R != 0
    }
    pub fn is_write(&self) -> bool {
        self.p_flags & PF_W != 0
    }
    pub fn is_executable(&self) -> bool {
        self.p_flags & PF_X != 0
    }
}

/// Result of minimal ELF parsing.
pub struct ParsedElf<'a> {
    pub header: Elf64Header,
    pub program_headers: Vec<Elf64Phdr>,
    pub data: &'a [u8],
}

/// Read a little-endian u16 from a byte slice.
fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

/// Read a little-endian u32 from a byte slice.
fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Read a little-endian u64 from a byte slice.
fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ])
}

/// Parse an ELF64 binary, reading only the ELF header and program headers.
///
/// Validates:
/// - ELF magic number
/// - 64-bit class
/// - Little-endian encoding
/// - Program header table is within file bounds
///
/// Does NOT parse: section headers, symbol tables, string tables, relocations,
/// dynamic linking info, or any other ELF structures.
pub fn parse_elf(data: &[u8]) -> Result<ParsedElf<'_>, ProcessError> {
    // ELF header is 64 bytes for ELF64
    const EHDR_SIZE: usize = 64;

    if data.len() < EHDR_SIZE {
        return Err(ProcessError::InvalidElf("file too small for ELF header"));
    }

    // Validate magic
    if data[0..4] != ELF_MAGIC {
        return Err(ProcessError::InvalidElf("invalid ELF magic number"));
    }

    // Validate class (must be ELF64)
    if data[4] != ELFCLASS64 {
        return Err(ProcessError::Not64Bit);
    }

    // Validate endianness (must be little-endian)
    if data[5] != ELFDATA2LSB {
        return Err(ProcessError::InvalidElf("unsupported ELF endianness (not little-endian)"));
    }

    let entry = read_u64_le(data, 24);    // e_entry
    let phoff = read_u64_le(data, 32);     // e_phoff
    let phentsize = read_u16_le(data, 54); // e_phentsize
    let phnum = read_u16_le(data, 56);     // e_phnum

    // Validate program header table bounds
    let phdr_end = (phoff as usize)
        .checked_add((phentsize as usize).checked_mul(phnum as usize).ok_or(
            ProcessError::InvalidElf("program header table size overflows"),
        )?)
        .ok_or(ProcessError::InvalidElf(
            "program header table offset + size overflows",
        ))?;

    if phdr_end > data.len() {
        return Err(ProcessError::InvalidElf(
            "program header table extends beyond file",
        ));
    }

    // Parse program headers (each is 56 bytes for ELF64, but use phentsize)
    let mut program_headers = Vec::with_capacity(phnum as usize);
    for i in 0..phnum as usize {
        let base = phoff as usize + i * phentsize as usize;
        if base + 56 > data.len() {
            return Err(ProcessError::InvalidElf(
                "program header extends beyond file",
            ));
        }

        program_headers.push(Elf64Phdr {
            p_type: read_u32_le(data, base),
            p_flags: read_u32_le(data, base + 4),
            p_offset: read_u64_le(data, base + 8),
            p_vaddr: read_u64_le(data, base + 16),
            p_filesz: read_u64_le(data, base + 32),
            p_memsz: read_u64_le(data, base + 40),
        });
    }

    Ok(ParsedElf {
        header: Elf64Header {
            entry,
            phoff,
            phentsize,
            phnum,
        },
        program_headers,
        data,
    })
}

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
    header: &Elf64Phdr,
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
    let segment_end = header.p_vaddr.checked_add(header.p_memsz).ok_or_else(|| {
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
    let file_end = header.p_offset.checked_add(header.p_filesz).ok_or_else(|| {
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
///
/// Uses the minimal ELF parser to read only the ELF header and program headers,
/// then maps PT_LOAD segments into the address space. Returns the entry point
/// and the list of mappings created.
pub fn load_elf(data: &[u8]) -> Result<(u64, Vec<Mapping>), ProcessError> {
    let elf = parse_elf(data)?;
    let mut mappings = Vec::new();
    let mut last_segment_end: u64 = 0;
    let mut last_segment_executable = false;

    let file_size = data.len();
    let file_ptr = data.as_ptr();

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
                            file_ptr.add(header.p_offset as usize),
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

            other => trace!("Ignoring program header type {}", other),
        }
    }

    Ok((elf.header.entry, mappings))
}
