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
