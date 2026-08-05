//! Minimal ELF64 parser for Panda OS.
//!
//! Reads only the ELF header and program headers, skipping section headers,
//! symbol tables, string tables, relocations, and dynamic linking info. This
//! is significantly faster than a full ELF parse (e.g., goblin), especially
//! in debug builds.
//!
//! # Usage
//!
//! ```ignore
//! let elf = panda_elf::parse_elf(data)?;
//! for phdr in &elf.program_headers {
//!     if phdr.p_type == panda_elf::PT_LOAD {
//!         // map segment...
//!     }
//! }
//! ```

#![no_std]

#[cfg(test)]
extern crate std;

/// ELF magic bytes: `\x7fELF`.
pub const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];

/// ELF class: 64-bit objects.
pub const ELFCLASS64: u8 = 2;

/// ELF data encoding: little-endian.
pub const ELFDATA2LSB: u8 = 1;

/// Program header type: loadable segment.
pub const PT_LOAD: u32 = 1;

/// Segment flag: executable.
pub const PF_X: u32 = 1;

/// Segment flag: writable.
pub const PF_W: u32 = 2;

/// Segment flag: readable.
pub const PF_R: u32 = 4;

/// Size of the ELF64 header in bytes.
pub const SIZEOF_EHDR: usize = 64;

/// Size of an ELF64 program header entry in bytes.
pub const SIZEOF_PHDR: usize = 56;

/// Errors returned when parsing an ELF binary.
#[derive(Debug)]
pub enum ElfError {
    /// File is too small to contain the expected structure.
    FileTooSmall,
    /// The ELF magic number is wrong.
    InvalidMagic,
    /// The binary is not 64-bit.
    Not64Bit,
    /// Unsupported endianness (only little-endian is supported).
    UnsupportedEndianness,
    /// Arithmetic overflow in header size calculations.
    Overflow(&'static str),
    /// A structure extends beyond the end of the file.
    OutOfBounds(&'static str),
}

/// Minimal ELF64 header — only the fields needed for loading.
#[derive(Debug)]
pub struct Elf64Header {
    /// Entry point virtual address.
    pub entry: u64,
    /// Program header table file offset.
    pub phoff: u64,
    /// Size of a program header table entry.
    pub phentsize: u16,
    /// Number of entries in the program header table.
    pub phnum: u16,
}

/// Minimal ELF64 program header — only the fields needed for loading.
#[derive(Debug)]
pub struct Elf64Phdr {
    /// Segment type (e.g., `PT_LOAD`).
    pub p_type: u32,
    /// Segment flags (combination of `PF_R`, `PF_W`, `PF_X`).
    pub p_flags: u32,
    /// Offset of the segment in the file.
    pub p_offset: u64,
    /// Virtual address of the segment in memory.
    pub p_vaddr: u64,
    /// Size of the segment in the file.
    pub p_filesz: u64,
    /// Size of the segment in memory (may be larger than `p_filesz` for BSS).
    pub p_memsz: u64,
}

impl Elf64Phdr {
    /// Whether the segment is readable.
    pub fn is_read(&self) -> bool {
        self.p_flags & PF_R != 0
    }
    /// Whether the segment is writable.
    pub fn is_write(&self) -> bool {
        self.p_flags & PF_W != 0
    }
    /// Whether the segment is executable.
    pub fn is_executable(&self) -> bool {
        self.p_flags & PF_X != 0
    }
}

/// Result of parsing an ELF64 binary.
pub struct ParsedElf<'a> {
    /// The ELF header.
    pub header: Elf64Header,
    /// The program headers (only `PT_LOAD` segments are relevant for loading).
    pub program_headers: &'a [Elf64Phdr],
    /// The raw ELF file data.
    pub data: &'a [u8],
}

/// Read a little-endian u16 from a byte slice at the given offset.
#[inline]
fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

/// Read a little-endian u32 from a byte slice at the given offset.
#[inline]
fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Read a little-endian u64 from a byte slice at the given offset.
#[inline]
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

/// Parse a single ELF64 program header from raw bytes.
///
/// `data` must be at least 56 bytes (SIZEOF_PHDR).
fn parse_phdr(data: &[u8]) -> Elf64Phdr {
    Elf64Phdr {
        p_type: read_u32_le(data, 0),
        p_flags: read_u32_le(data, 4),
        p_offset: read_u64_le(data, 8),
        p_vaddr: read_u64_le(data, 16),
        p_filesz: read_u64_le(data, 32),
        p_memsz: read_u64_le(data, 40),
    }
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
///
/// The returned `ParsedElf` borrows from `buf`, which must be large enough to
/// hold both the raw data and the parsed program header array. Use
/// [`program_headers_buf_len`] to determine the required buffer size.
///
/// # Arguments
/// * `data` - The raw ELF file bytes.
/// * `buf`  - Scratch buffer for storing parsed program headers. Must have
///            length >= `phnum` (call [`program_headers_buf_len`] first, or
///            provide a conservatively large buffer).
pub fn parse_elf<'a>(
    data: &'a [u8],
    buf: &'a mut [Elf64Phdr],
) -> Result<ParsedElf<'a>, ElfError> {
    if data.len() < SIZEOF_EHDR {
        return Err(ElfError::FileTooSmall);
    }

    // Validate magic
    if data[0..4] != ELF_MAGIC {
        return Err(ElfError::InvalidMagic);
    }

    // Validate class (must be ELF64)
    if data[4] != ELFCLASS64 {
        return Err(ElfError::Not64Bit);
    }

    // Validate endianness (must be little-endian)
    if data[5] != ELFDATA2LSB {
        return Err(ElfError::UnsupportedEndianness);
    }

    let entry = read_u64_le(data, 24);     // e_entry
    let phoff = read_u64_le(data, 32);      // e_phoff
    let phentsize = read_u16_le(data, 54);  // e_phentsize
    let phnum = read_u16_le(data, 56);      // e_phnum

    // Validate program header table bounds
    let phdr_end = (phoff as usize)
        .checked_add(
            (phentsize as usize)
                .checked_mul(phnum as usize)
                .ok_or(ElfError::Overflow("program header table size overflows"))?,
        )
        .ok_or(ElfError::Overflow(
            "program header table offset + size overflows",
        ))?;

    if phdr_end > data.len() {
        return Err(ElfError::OutOfBounds(
            "program header table extends beyond file",
        ));
    }

    // Parse program headers
    let count = phnum as usize;
    if buf.len() < count {
        return Err(ElfError::OutOfBounds(
            "program header buffer too small",
        ));
    }

    for i in 0..count {
        let base = phoff as usize + i * phentsize as usize;
        if base + SIZEOF_PHDR > data.len() {
            return Err(ElfError::OutOfBounds(
                "program header extends beyond file",
            ));
        }
        buf[i] = parse_phdr(&data[base..]);
    }

    Ok(ParsedElf {
        header: Elf64Header {
            entry,
            phoff,
            phentsize,
            phnum,
        },
        program_headers: &buf[..count],
        data,
    })
}

/// Offset of `e_shoff` (section header table file offset) in the ELF64 header.
const E_SHOFF_OFFSET: usize = 40;
/// Offset of `e_shentsize` (section header entry size) in the ELF64 header.
const E_SHENTSIZE_OFFSET: usize = 58;
/// Offset of `e_shnum` (number of section headers) in the ELF64 header.
const E_SHNUM_OFFSET: usize = 60;
/// Offset of `e_shstrndx` (section header string table index) in the ELF64 header.
const E_SHSTRNDX_OFFSET: usize = 62;

/// Size of an ELF64 section header entry in bytes.
const SIZEOF_SHDR: usize = 64;

/// Offset of `sh_name` within a section header entry.
const SH_NAME_OFFSET: usize = 0;
/// Offset of `sh_offset` within a section header entry.
const SH_OFFSET_OFFSET: usize = 24;
/// Offset of `sh_size` within a section header entry.
const SH_SIZE_OFFSET: usize = 32;

/// Read one section header entry's `(name_off, offset, size)` fields.
fn read_shdr_fields(data: &[u8], base: usize) -> Option<(u32, u64, u64)> {
    if base + SIZEOF_SHDR > data.len() {
        return None;
    }
    let name_off = read_u32_le(data, base + SH_NAME_OFFSET);
    let offset = read_u64_le(data, base + SH_OFFSET_OFFSET);
    let size = read_u64_le(data, base + SH_SIZE_OFFSET);
    Some((name_off, offset, size))
}

/// Look up a byte slice `[start, start+len)` in `data`, checked for overflow
/// and bounds.
fn slice_at(data: &[u8], start: u64, len: u64) -> Option<&[u8]> {
    let start = usize::try_from(start).ok()?;
    let len = usize::try_from(len).ok()?;
    let end = start.checked_add(len)?;
    if end > data.len() {
        return None;
    }
    Some(&data[start..end])
}

/// Read a null-terminated string starting at `offset` within `strtab`.
fn strtab_str_at(strtab: &[u8], offset: u32) -> Option<&[u8]> {
    let offset = offset as usize;
    if offset > strtab.len() {
        return None;
    }
    let rest = &strtab[offset..];
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Read the contents of a named ELF section, without parsing program
/// headers, symbol tables, relocations, or any other ELF structures.
///
/// Returns `None` if the ELF header is malformed or truncated, if there is
/// no section header table, if the section header string table is missing
/// or out of bounds, or if no section with the given `name` is found. Never
/// panics on malformed or truncated input.
///
/// Used to read driver device-match tables from `.panda_devices.<bus>`
/// sections without executing the driver binary.
pub fn read_section<'a>(elf_bytes: &'a [u8], name: &str) -> Option<&'a [u8]> {
    if elf_bytes.len() < SIZEOF_EHDR {
        return None;
    }
    if elf_bytes[0..4] != ELF_MAGIC {
        return None;
    }
    if elf_bytes[4] != ELFCLASS64 {
        return None;
    }
    if elf_bytes[5] != ELFDATA2LSB {
        return None;
    }

    let shoff = read_u64_le(elf_bytes, E_SHOFF_OFFSET);
    let shentsize = read_u16_le(elf_bytes, E_SHENTSIZE_OFFSET) as usize;
    let shnum = read_u16_le(elf_bytes, E_SHNUM_OFFSET) as usize;
    let shstrndx = read_u16_le(elf_bytes, E_SHSTRNDX_OFFSET) as usize;

    if shnum == 0 || shentsize < SIZEOF_SHDR {
        return None;
    }

    // Locate the section header string table entry, then its bytes.
    let strtab_shdr_base = shoff
        .checked_add((shstrndx.checked_mul(shentsize)?) as u64)?;
    let strtab_shdr_base = usize::try_from(strtab_shdr_base).ok()?;
    let (_, strtab_off, strtab_size) = read_shdr_fields(elf_bytes, strtab_shdr_base)?;
    let strtab = slice_at(elf_bytes, strtab_off, strtab_size)?;

    for i in 0..shnum {
        let base = (shoff as usize).checked_add(i.checked_mul(shentsize)?)?;
        let (name_off, sh_offset, sh_size) = read_shdr_fields(elf_bytes, base)?;
        let sh_name = strtab_str_at(strtab, name_off)?;
        if sh_name == name.as_bytes() {
            return slice_at(elf_bytes, sh_offset, sh_size);
        }
    }

    None
}

/// Returns the number of program headers declared in the ELF header.
///
/// Call this before [`parse_elf`] to know how large a buffer to allocate.
/// Returns `None` if the data is too small for an ELF header or has an invalid
/// magic number.
pub fn program_headers_count(data: &[u8]) -> Option<usize> {
    if data.len() < SIZEOF_EHDR {
        return None;
    }
    if data[0..4] != ELF_MAGIC {
        return None;
    }
    Some(read_u16_le(data, 56) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// Build a minimal ELF64 relocatable object with a single named section
    /// containing `contents`, plus a `.shstrtab` string table section.
    /// Returns the file bytes.
    fn build_elf_with_section(section_name: &str, contents: &[u8]) -> Vec<u8> {
        // Layout: [ehdr][section data][shstrtab][section headers]
        let mut buf = vec![0u8; SIZEOF_EHDR];

        // e_ident
        buf[0..4].copy_from_slice(&ELF_MAGIC);
        buf[4] = ELFCLASS64;
        buf[5] = ELFDATA2LSB;

        let data_off = buf.len() as u64;
        buf.extend_from_slice(contents);

        // shstrtab: index 0 is the empty string (conventional), then
        // "\0.panda_devices.pci\0" etc.
        let shstrtab_off = buf.len() as u64;
        let mut shstrtab = vec![0u8]; // NUL for empty string at offset 0
        let name_off = shstrtab.len() as u32;
        shstrtab.extend_from_slice(section_name.as_bytes());
        shstrtab.push(0);
        buf.extend_from_slice(&shstrtab);
        let shstrtab_size = shstrtab.len() as u64;

        // Section headers: [0] = NULL section, [1] = our section, [2] = shstrtab
        let shoff = buf.len() as u64;

        let write_shdr = |buf: &mut Vec<u8>, name: u32, offset: u64, size: u64| {
            let mut shdr = vec![0u8; SIZEOF_SHDR];
            shdr[SH_NAME_OFFSET..SH_NAME_OFFSET + 4].copy_from_slice(&name.to_le_bytes());
            shdr[SH_OFFSET_OFFSET..SH_OFFSET_OFFSET + 8].copy_from_slice(&offset.to_le_bytes());
            shdr[SH_SIZE_OFFSET..SH_SIZE_OFFSET + 8].copy_from_slice(&size.to_le_bytes());
            buf.extend_from_slice(&shdr);
        };

        write_shdr(&mut buf, 0, 0, 0); // NULL section
        write_shdr(&mut buf, name_off, data_off, contents.len() as u64);
        write_shdr(&mut buf, 0, shstrtab_off, shstrtab_size);

        // e_shoff, e_shentsize, e_shnum, e_shstrndx
        buf[E_SHOFF_OFFSET..E_SHOFF_OFFSET + 8].copy_from_slice(&shoff.to_le_bytes());
        buf[E_SHENTSIZE_OFFSET..E_SHENTSIZE_OFFSET + 2]
            .copy_from_slice(&(SIZEOF_SHDR as u16).to_le_bytes());
        buf[E_SHNUM_OFFSET..E_SHNUM_OFFSET + 2].copy_from_slice(&3u16.to_le_bytes());
        buf[E_SHSTRNDX_OFFSET..E_SHSTRNDX_OFFSET + 2].copy_from_slice(&2u16.to_le_bytes());

        buf
    }

    #[test]
    fn correct_section_returns_correct_bytes() {
        let contents = [0xAAu8, 0xBB, 0xCC, 0xDD, 0x11, 0x22, 0x33, 0x44];
        let elf = build_elf_with_section(".panda_devices.pci", &contents);
        let section = read_section(&elf, ".panda_devices.pci");
        assert_eq!(section, Some(&contents[..]));
    }

    #[test]
    fn absent_section_returns_none() {
        let contents = [1u8, 2, 3, 4];
        let elf = build_elf_with_section(".panda_devices.pci", &contents);
        assert_eq!(read_section(&elf, ".panda_devices.usb"), None);
    }

    #[test]
    fn truncated_elf_returns_none() {
        let contents = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let elf = build_elf_with_section(".panda_devices.pci", &contents);
        // Truncate to just the ELF header - no section header table.
        let truncated = &elf[..SIZEOF_EHDR];
        assert_eq!(read_section(truncated, ".panda_devices.pci"), None);

        // Truncate to nothing at all.
        assert_eq!(read_section(&[], ".panda_devices.pci"), None);

        // Truncate right before the section header table ends.
        let almost = &elf[..elf.len() - 1];
        assert_eq!(read_section(almost, ".panda_devices.pci"), None);
    }

    #[test]
    fn wrong_magic_returns_none() {
        let contents = [1u8, 2, 3, 4];
        let mut elf = build_elf_with_section(".panda_devices.pci", &contents);
        elf[0] = 0x00; // corrupt magic
        assert_eq!(read_section(&elf, ".panda_devices.pci"), None);
    }
}
