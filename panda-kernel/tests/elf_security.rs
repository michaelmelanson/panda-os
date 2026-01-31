#![no_std]
#![no_main]

use panda_kernel::process::{Context, Process, ProcessError};

panda_kernel::test_harness!(
    test_reject_kernel_space_vaddr,
    test_reject_vaddr_memsz_overflow,
    test_reject_vaddr_memsz_kernel_space,
    test_reject_offset_filesz_overflow,
    test_reject_offset_exceeds_file_size,
    test_accept_valid_elf
);

/// Helper to create a minimal valid ELF header
fn create_minimal_elf_header() -> [u8; 0x200] {
    let mut elf = [0u8; 0x200];

    // ELF magic number
    elf[0] = 0x7f;
    elf[1] = b'E';
    elf[2] = b'L';
    elf[3] = b'F';

    // 64-bit
    elf[4] = 2;

    // Little endian
    elf[5] = 1;

    // ELF version
    elf[6] = 1;

    // Type: executable (at offset 16, 2 bytes little endian)
    elf[16] = 2;
    elf[17] = 0;

    // Machine: x86-64 (at offset 18, 2 bytes little endian)
    elf[18] = 0x3e;
    elf[19] = 0;

    // Version (at offset 20, 4 bytes)
    elf[20] = 1;

    // Entry point (at offset 24, 8 bytes) - valid userspace address
    let entry: u64 = 0x400000;
    elf[24..32].copy_from_slice(&entry.to_le_bytes());

    // Program header offset (at offset 32, 8 bytes) - starts at 0x40
    let phoff: u64 = 0x40;
    elf[32..40].copy_from_slice(&phoff.to_le_bytes());

    // Section header offset (at offset 40, 8 bytes) - none
    elf[40..48].copy_from_slice(&0u64.to_le_bytes());

    // Header size (at offset 52, 2 bytes) - 64 bytes
    elf[52] = 64;

    // Program header entry size (at offset 54, 2 bytes) - 56 bytes
    elf[54] = 56;

    // Program header count (at offset 56, 2 bytes) - 1 entry
    elf[56] = 1;

    elf
}

/// Create a program header at the given offset
fn set_program_header(
    elf: &mut [u8],
    offset: usize,
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
) {
    elf[offset..offset + 4].copy_from_slice(&p_type.to_le_bytes());
    elf[offset + 4..offset + 8].copy_from_slice(&p_flags.to_le_bytes());
    elf[offset + 8..offset + 16].copy_from_slice(&p_offset.to_le_bytes());
    elf[offset + 16..offset + 24].copy_from_slice(&p_vaddr.to_le_bytes());
    elf[offset + 24..offset + 32].copy_from_slice(&p_paddr.to_le_bytes());
    elf[offset + 32..offset + 40].copy_from_slice(&p_filesz.to_le_bytes());
    elf[offset + 40..offset + 48].copy_from_slice(&p_memsz.to_le_bytes());
    elf[offset + 48..offset + 56].copy_from_slice(&p_align.to_le_bytes());
}

/// Test that ELF with p_vaddr in kernel space is rejected
fn test_reject_kernel_space_vaddr() {
    let mut elf = create_minimal_elf_header();

    // PT_LOAD segment with p_vaddr in kernel space
    const PT_LOAD: u32 = 1;
    const PF_R: u32 = 4;

    set_program_header(
        &mut elf,
        0x40, // program header offset
        PT_LOAD,
        PF_R,
        0x100,                    // p_offset - valid offset in file
        0xffff_8000_0000_0000,    // p_vaddr - kernel space address
        0xffff_8000_0000_0000,    // p_paddr
        0x100,                    // p_filesz
        0x100,                    // p_memsz
        0x1000,                   // p_align
    );

    let context = Context::new();
    let result = Process::from_elf_data(context, &elf as *const [u8]);

    assert!(result.is_err(), "Should reject ELF with kernel space p_vaddr");
    match result {
        Err(ProcessError::InvalidElf(msg)) => {
            assert!(msg.contains("kernel space"), "Error message should mention kernel space");
        }
        _ => panic!("Wrong error type"),
    }
}

/// Test that ELF with p_vaddr + p_memsz overflow is rejected
fn test_reject_vaddr_memsz_overflow() {
    let mut elf = create_minimal_elf_header();

    const PT_LOAD: u32 = 1;
    const PF_R: u32 = 4;

    set_program_header(
        &mut elf,
        0x40,
        PT_LOAD,
        PF_R,
        0x100,
        0x0000_7fff_ffff_0000,    // p_vaddr - high userspace address
        0x0000_7fff_ffff_0000,
        0x100,
        0x10000,                  // p_memsz - causes overflow
        0x1000,
    );

    let context = Context::new();
    let result = Process::from_elf_data(context, &elf as *const [u8]);

    assert!(result.is_err(), "Should reject ELF with p_vaddr + p_memsz overflow");
    match result {
        Err(ProcessError::InvalidElf(msg)) => {
            assert!(
                msg.contains("overflow") || msg.contains("kernel space"),
                "Error message should mention overflow or kernel space"
            );
        }
        _ => panic!("Wrong error type"),
    }
}

/// Test that ELF with p_vaddr + p_memsz extending into kernel space is rejected
fn test_reject_vaddr_memsz_kernel_space() {
    let mut elf = create_minimal_elf_header();

    const PT_LOAD: u32 = 1;
    const PF_R: u32 = 4;

    set_program_header(
        &mut elf,
        0x40,
        PT_LOAD,
        PF_R,
        0x100,
        0x0000_7fff_ffff_0000,    // p_vaddr - valid userspace
        0x0000_7fff_ffff_0000,
        0x100,
        0x2000,                   // p_memsz - extends past USER_ADDR_MAX
        0x1000,
    );

    let context = Context::new();
    let result = Process::from_elf_data(context, &elf as *const [u8]);

    assert!(result.is_err(), "Should reject ELF with segment extending into kernel space");
    match result {
        Err(ProcessError::InvalidElf(msg)) => {
            assert!(msg.contains("kernel space"), "Error message should mention kernel space");
        }
        _ => panic!("Wrong error type"),
    }
}

/// Test that ELF with p_offset + p_filesz overflow is rejected
fn test_reject_offset_filesz_overflow() {
    let mut elf = create_minimal_elf_header();

    const PT_LOAD: u32 = 1;
    const PF_R: u32 = 4;

    set_program_header(
        &mut elf,
        0x40,
        PT_LOAD,
        PF_R,
        0xffff_ffff_ffff_0000,    // p_offset - high value
        0x400000,                 // p_vaddr - valid
        0x400000,
        0x10000,                  // p_filesz - causes overflow
        0x10000,
        0x1000,
    );

    let context = Context::new();
    let result = Process::from_elf_data(context, &elf as *const [u8]);

    assert!(result.is_err(), "Should reject ELF with p_offset + p_filesz overflow");
    match result {
        Err(ProcessError::InvalidElf(msg)) => {
            assert!(msg.contains("overflow"), "Error message should mention overflow");
        }
        _ => panic!("Wrong error type"),
    }
}

/// Test that ELF with p_offset + p_filesz exceeding file size is rejected
fn test_reject_offset_exceeds_file_size() {
    let mut elf = create_minimal_elf_header();

    const PT_LOAD: u32 = 1;
    const PF_R: u32 = 4;

    set_program_header(
        &mut elf,
        0x40,
        PT_LOAD,
        PF_R,
        0x100,                    // p_offset
        0x400000,                 // p_vaddr - valid
        0x400000,
        0x1000,                   // p_filesz - exceeds file size (0x200)
        0x1000,
        0x1000,
    );

    let context = Context::new();
    let result = Process::from_elf_data(context, &elf as *const [u8]);

    assert!(result.is_err(), "Should reject ELF with segment offset exceeding file size");
    match result {
        Err(ProcessError::InvalidElf(msg)) => {
            assert!(msg.contains("file size"), "Error message should mention file size");
        }
        _ => panic!("Wrong error type"),
    }
}

/// Test that a valid ELF with proper constraints is still accepted
fn test_accept_valid_elf() {
    let mut elf = create_minimal_elf_header();

    const PT_LOAD: u32 = 1;
    const PF_R: u32 = 4;
    const PF_X: u32 = 1;

    set_program_header(
        &mut elf,
        0x40,
        PT_LOAD,
        PF_R | PF_X,              // readable and executable
        0x100,                    // p_offset - within file
        0x400000,                 // p_vaddr - valid userspace address
        0x400000,
        0x80,                     // p_filesz - within file bounds
        0x1000,                   // p_memsz - valid
        0x1000,
    );

    let context = Context::new();
    let result = Process::from_elf_data(context, &elf as *const [u8]);

    assert!(result.is_ok(), "Should accept valid ELF with proper constraints");
}
