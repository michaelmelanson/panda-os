//! Security tests for ELF loading.
//!
//! These tests verify that the ELF loader correctly validates segment addresses
//! and file offsets to prevent kernel memory corruption attacks.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use goblin::elf::Elf;
use goblin::elf::header::{ELFMAG, SELFMAG, EI_CLASS, EI_DATA, EI_VERSION, ELFCLASS64, ELFDATA2LSB, EV_CURRENT, ET_EXEC, EM_X86_64};
use goblin::elf::program_header::{PT_LOAD, PF_R, PF_X};
use goblin::elf64::header::SIZEOF_EHDR;
use goblin::elf64::program_header::SIZEOF_PHDR;
use panda_kernel::process::ProcessError;

panda_kernel::test_harness!(
    test_reject_kernel_space_vaddr,
    test_reject_vaddr_memsz_overflow,
    test_reject_vaddr_memsz_kernel_space,
    test_reject_offset_filesz_overflow,
    test_reject_offset_exceeds_file_size,
    test_accept_valid_elf,
);

/// Helper to create a minimal ELF header using goblin constants.
fn create_elf_header() -> [u8; SIZEOF_EHDR] {
    let mut header = [0u8; SIZEOF_EHDR];

    // ELF magic number
    header[0..SELFMAG].copy_from_slice(ELFMAG);
    header[EI_CLASS] = ELFCLASS64;
    header[EI_DATA] = ELFDATA2LSB;
    header[EI_VERSION] = EV_CURRENT;

    // e_type: ET_EXEC (executable)
    header[16..18].copy_from_slice(&ET_EXEC.to_le_bytes());

    // e_machine: EM_X86_64
    header[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());

    // e_version
    header[20..24].copy_from_slice(&(EV_CURRENT as u32).to_le_bytes());

    // e_entry
    header[24..32].copy_from_slice(&0x400000u64.to_le_bytes());

    // e_phoff (program header offset - right after ELF header)
    header[32..40].copy_from_slice(&(SIZEOF_EHDR as u64).to_le_bytes());

    // e_shoff (section header offset - 0 for minimal binary)
    header[40..48].copy_from_slice(&0u64.to_le_bytes());

    // e_flags
    header[48..52].copy_from_slice(&0u32.to_le_bytes());

    // e_ehsize (ELF header size)
    header[52..54].copy_from_slice(&(SIZEOF_EHDR as u16).to_le_bytes());

    // e_phentsize (program header entry size)
    header[54..56].copy_from_slice(&(SIZEOF_PHDR as u16).to_le_bytes());

    // e_phnum (number of program headers)
    header[56..58].copy_from_slice(&1u16.to_le_bytes());

    header
}

/// Helper to create a PT_LOAD program header using goblin constants.
fn create_program_header(p_vaddr: u64, p_memsz: u64, p_offset: u64, p_filesz: u64) -> [u8; SIZEOF_PHDR] {
    let mut phdr = [0u8; SIZEOF_PHDR];

    // p_type: PT_LOAD
    phdr[0..4].copy_from_slice(&PT_LOAD.to_le_bytes());

    // p_flags: PF_R | PF_X (readable + executable)
    phdr[4..8].copy_from_slice(&(PF_R | PF_X).to_le_bytes());

    // p_offset
    phdr[8..16].copy_from_slice(&p_offset.to_le_bytes());

    // p_vaddr
    phdr[16..24].copy_from_slice(&p_vaddr.to_le_bytes());

    // p_paddr (not used, set to p_vaddr)
    phdr[24..32].copy_from_slice(&p_vaddr.to_le_bytes());

    // p_filesz
    phdr[32..40].copy_from_slice(&p_filesz.to_le_bytes());

    // p_memsz
    phdr[40..48].copy_from_slice(&p_memsz.to_le_bytes());

    // p_align
    phdr[48..56].copy_from_slice(&0x1000u64.to_le_bytes());

    phdr
}

fn test_reject_kernel_space_vaddr() {
    // Craft an ELF with p_vaddr in kernel space (0xffff_8000_0000_0000)
    let mut elf_data = vec![0u8; 4096];

    let header = create_elf_header();
    elf_data[0..SIZEOF_EHDR].copy_from_slice(&header);

    let phdr = create_program_header(
        0xffff_8000_0000_0000, // kernel space address
        0x1000,                 // 4KB size
        128,                    // file offset
        0x1000,                 // file size
    );
    elf_data[SIZEOF_EHDR..SIZEOF_EHDR + SIZEOF_PHDR].copy_from_slice(&phdr);

    let elf = Elf::parse(&elf_data).expect("Failed to parse crafted ELF");

    let result = panda_kernel::process::elf::load_elf(&elf, elf_data.as_slice() as *const [u8]);

    match result {
        Err(ProcessError::InvalidElf(msg)) => {
            assert!(msg.contains("kernel space") || msg.contains("exceeds"));
        }
        _ => panic!("Expected InvalidElf error for kernel space vaddr"),
    }
}

fn test_reject_vaddr_memsz_overflow() {
    // Craft an ELF where p_vaddr + p_memsz overflows u64
    let mut elf_data = vec![0u8; 4096];

    let header = create_elf_header();
    elf_data[0..SIZEOF_EHDR].copy_from_slice(&header);

    let phdr = create_program_header(
        0x0000_0000_0040_0000, // valid userspace address
        u64::MAX,               // huge memsz that causes u64 overflow when added to p_vaddr
        128,
        0x1000,
    );
    elf_data[SIZEOF_EHDR..SIZEOF_EHDR + SIZEOF_PHDR].copy_from_slice(&phdr);

    let elf = Elf::parse(&elf_data).expect("Failed to parse crafted ELF");

    let result = panda_kernel::process::elf::load_elf(&elf, elf_data.as_slice() as *const [u8]);

    match result {
        Err(ProcessError::InvalidElf(msg)) => {
            assert!(msg.contains("overflow") || msg.contains("exceeds"));
        }
        _ => panic!("Expected InvalidElf error for vaddr+memsz overflow"),
    }
}

fn test_reject_vaddr_memsz_kernel_space() {
    // Craft an ELF where p_vaddr is in userspace but p_vaddr + p_memsz extends into kernel
    let mut elf_data = vec![0u8; 4096];

    let header = create_elf_header();
    elf_data[0..SIZEOF_EHDR].copy_from_slice(&header);

    let phdr = create_program_header(
        0x0000_7fff_ffff_0000, // valid userspace
        0x20000,                // extends past USER_ADDR_MAX
        128,
        0x1000,
    );
    elf_data[SIZEOF_EHDR..SIZEOF_EHDR + SIZEOF_PHDR].copy_from_slice(&phdr);

    let elf = Elf::parse(&elf_data).expect("Failed to parse crafted ELF");

    let result = panda_kernel::process::elf::load_elf(&elf, elf_data.as_slice() as *const [u8]);

    match result {
        Err(ProcessError::InvalidElf(msg)) => {
            assert!(msg.contains("kernel space") || msg.contains("exceeds"));
        }
        _ => panic!("Expected InvalidElf error for segment extending into kernel space"),
    }
}

fn test_reject_offset_filesz_overflow() {
    // Craft an ELF where p_offset + p_filesz overflows
    let mut elf_data = vec![0u8; 4096];

    let header = create_elf_header();
    elf_data[0..SIZEOF_EHDR].copy_from_slice(&header);

    let phdr = create_program_header(
        0x400000,              // valid address
        0x1000,                 // valid size
        0xffff_ffff_ffff_ff00, // offset near max
        0x200,                  // causes overflow
    );
    elf_data[SIZEOF_EHDR..SIZEOF_EHDR + SIZEOF_PHDR].copy_from_slice(&phdr);

    let elf = Elf::parse(&elf_data).expect("Failed to parse crafted ELF");

    let result = panda_kernel::process::elf::load_elf(&elf, elf_data.as_slice() as *const [u8]);

    match result {
        Err(ProcessError::InvalidElf(msg)) => {
            assert!(msg.contains("overflow"));
        }
        _ => panic!("Expected InvalidElf error for offset+filesz overflow"),
    }
}

fn test_reject_offset_exceeds_file_size() {
    // Craft an ELF where p_offset + p_filesz exceeds the actual file size
    let mut elf_data = vec![0u8; 4096];

    let header = create_elf_header();
    elf_data[0..SIZEOF_EHDR].copy_from_slice(&header);

    let phdr = create_program_header(
        0x400000,  // valid address
        0x2000,     // memory size
        0x200,      // file offset
        0x2000,     // file size that exceeds actual file (4096 - 0x200 = 3840 bytes available)
    );
    elf_data[SIZEOF_EHDR..SIZEOF_EHDR + SIZEOF_PHDR].copy_from_slice(&phdr);

    let elf = Elf::parse(&elf_data).expect("Failed to parse crafted ELF");

    let result = panda_kernel::process::elf::load_elf(&elf, elf_data.as_slice() as *const [u8]);

    match result {
        Err(ProcessError::InvalidElf(msg)) => {
            assert!(msg.contains("file size") || msg.contains("offset exceeds"));
        }
        _ => panic!("Expected InvalidElf error for offset exceeding file size"),
    }
}

fn test_accept_valid_elf() {
    // Craft a valid ELF that should be accepted
    let mut elf_data = vec![0u8; 4096];

    let header = create_elf_header();
    elf_data[0..SIZEOF_EHDR].copy_from_slice(&header);

    let phdr = create_program_header(
        0x400000,  // valid userspace address
        0x1000,     // 4KB memory size
        128,        // valid offset
        0x100,      // valid file size
    );
    elf_data[SIZEOF_EHDR..SIZEOF_EHDR + SIZEOF_PHDR].copy_from_slice(&phdr);

    // Add some dummy code data
    for i in 0..0x100 {
        elf_data[128 + i] = (i % 256) as u8;
    }

    let elf = Elf::parse(&elf_data).expect("Failed to parse crafted ELF");

    let result = panda_kernel::process::elf::load_elf(&elf, elf_data.as_slice() as *const [u8]);

    match result {
        Ok(mappings) => {
            assert_eq!(mappings.len(), 1, "Expected 1 mapping for valid ELF");
        }
        Err(e) => panic!("Valid ELF should be accepted, got error: {:?}", e),
    }
}
