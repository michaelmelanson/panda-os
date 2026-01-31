//! Tests for ELF loader security validations.
//!
//! These tests verify that the ELF loader correctly rejects malicious binaries
//! that attempt to:
//! - Map segments at kernel addresses
//! - Trigger integer overflows in address calculations
//! - Read beyond the bounds of the ELF file

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use panda_kernel::{eprintln, print, println};
use panda_kernel::process::{Context, Process, ProcessError};

/// Minimal ELF64 header for testing (only the fields we need)
#[repr(C)]
struct Elf64Header {
    e_ident: [u8; 16],      // ELF identification
    e_type: u16,            // Object file type
    e_machine: u16,         // Machine type
    e_version: u32,         // Object file version
    e_entry: u64,           // Entry point address
    e_phoff: u64,           // Program header offset
    e_shoff: u64,           // Section header offset
    e_flags: u32,           // Processor-specific flags
    e_ehsize: u16,          // ELF header size
    e_phentsize: u16,       // Program header entry size
    e_phnum: u16,           // Number of program header entries
    e_shentsize: u16,       // Section header entry size
    e_shnum: u16,           // Number of section header entries
    e_shstrndx: u16,        // Section name string table index
}

/// ELF64 program header
#[repr(C)]
struct Elf64ProgramHeader {
    p_type: u32,            // Segment type
    p_flags: u32,           // Segment flags
    p_offset: u64,          // Offset in file
    p_vaddr: u64,           // Virtual address
    p_paddr: u64,           // Physical address (unused)
    p_filesz: u64,          // Size in file
    p_memsz: u64,           // Size in memory
    p_align: u64,           // Alignment
}

const PT_LOAD: u32 = 1;
const PF_R: u32 = 4;
const PF_W: u32 = 2;
const PF_X: u32 = 1;

/// Create a minimal valid ELF binary for testing
fn create_test_elf(p_vaddr: u64, p_memsz: u64, p_offset: u64, p_filesz: u64) -> Vec<u8> {
    let mut data = Vec::new();

    // ELF header
    let header = Elf64Header {
        e_ident: [
            0x7f, b'E', b'L', b'F', // Magic number
            2,    // 64-bit
            1,    // Little-endian
            1,    // ELF version
            0,    // System V ABI
            0, 0, 0, 0, 0, 0, 0, 0, // Padding
        ],
        e_type: 2,              // ET_EXEC
        e_machine: 0x3e,        // x86-64
        e_version: 1,
        e_entry: 0x1000,
        e_phoff: 64,            // Program headers start after ELF header
        e_shoff: 0,
        e_flags: 0,
        e_ehsize: 64,
        e_phentsize: 56,        // Size of program header
        e_phnum: 1,             // One program header
        e_shentsize: 0,
        e_shnum: 0,
        e_shstrndx: 0,
    };

    // Convert header to bytes
    unsafe {
        let header_bytes = core::slice::from_raw_parts(
            &header as *const _ as *const u8,
            core::mem::size_of::<Elf64Header>(),
        );
        data.extend_from_slice(header_bytes);
    }

    // Program header
    let program_header = Elf64ProgramHeader {
        p_type: PT_LOAD,
        p_flags: PF_R | PF_X,
        p_offset,
        p_vaddr,
        p_paddr: p_vaddr,
        p_filesz,
        p_memsz,
        p_align: 0x1000,
    };

    // Convert program header to bytes
    unsafe {
        let ph_bytes = core::slice::from_raw_parts(
            &program_header as *const _ as *const u8,
            core::mem::size_of::<Elf64ProgramHeader>(),
        );
        data.extend_from_slice(ph_bytes);
    }

    // Pad with zeros to reach p_offset + p_filesz if needed
    let target_size = (p_offset + p_filesz).min(4096) as usize;
    while data.len() < target_size {
        data.push(0);
    }

    data
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    print!("test elf_security::test_reject_kernel_space_vaddr ... ");
    test_reject_kernel_space_vaddr();
    println!("PASS");

    print!("test elf_security::test_reject_vaddr_memsz_overflow ... ");
    test_reject_vaddr_memsz_overflow();
    println!("PASS");

    print!("test elf_security::test_reject_vaddr_memsz_kernel_space ... ");
    test_reject_vaddr_memsz_kernel_space();
    println!("PASS");

    print!("test elf_security::test_reject_offset_filesz_overflow ... ");
    test_reject_offset_filesz_overflow();
    println!("PASS");

    print!("test elf_security::test_reject_offset_exceeds_file_size ... ");
    test_reject_offset_exceeds_file_size();
    println!("PASS");

    print!("test elf_security::test_accept_valid_elf ... ");
    test_accept_valid_elf();
    println!("PASS");

    panda_kernel::process::exit(0);
}

/// Test that ELF segments with p_vaddr in kernel space are rejected
fn test_reject_kernel_space_vaddr() {
    let context = Context::new();

    // Attempt to map at kernel space address
    let data = create_test_elf(
        0xffff_8000_0000_0000,  // Kernel space address
        0x1000,
        0x200,
        0x100,
    );

    let result = Process::from_elf_data(context, &data as *const _);
    match result {
        Err(ProcessError::InvalidElf(msg)) => {
            assert!(msg.contains("kernel space"), "Expected kernel space error, got: {}", msg);
        }
        _ => panic!("Expected InvalidElf error for kernel space address"),
    }
}

/// Test that p_vaddr + p_memsz overflow is detected
fn test_reject_vaddr_memsz_overflow() {
    let context = Context::new();

    // Trigger overflow: vaddr + memsz wraps around
    let data = create_test_elf(
        0x0000_7fff_ffff_f000,
        0x2000,  // This will overflow when added to vaddr
        0x200,
        0x100,
    );

    let result = Process::from_elf_data(context, &data as *const _);
    match result {
        Err(ProcessError::InvalidElf(msg)) => {
            assert!(msg.contains("overflow"), "Expected overflow error, got: {}", msg);
        }
        _ => panic!("Expected InvalidElf error for address overflow"),
    }
}

/// Test that segments extending into kernel space are rejected
fn test_reject_vaddr_memsz_kernel_space() {
    let context = Context::new();

    // Segment starts in userspace but extends into kernel space
    let data = create_test_elf(
        0x0000_7fff_ffff_0000,
        0x20000,  // Extends well into kernel space
        0x200,
        0x100,
    );

    let result = Process::from_elf_data(context, &data as *const _);
    match result {
        Err(ProcessError::InvalidElf(msg)) => {
            assert!(msg.contains("kernel space"), "Expected kernel space error, got: {}", msg);
        }
        _ => panic!("Expected InvalidElf error for segment extending into kernel space"),
    }
}

/// Test that p_offset + p_filesz overflow is detected
fn test_reject_offset_filesz_overflow() {
    let context = Context::new();

    // Trigger overflow in file offset calculation
    let data = create_test_elf(
        0x1000,
        0x1000,
        u64::MAX - 0x100,  // Near max value
        0x200,             // Will overflow when added
    );

    let result = Process::from_elf_data(context, &data as *const _);
    match result {
        Err(ProcessError::InvalidElf(msg)) => {
            assert!(msg.contains("overflow"), "Expected overflow error, got: {}", msg);
        }
        _ => panic!("Expected InvalidElf error for file offset overflow"),
    }
}

/// Test that out-of-bounds file reads are rejected
fn test_reject_offset_exceeds_file_size() {
    let context = Context::new();

    // p_offset + p_filesz exceeds actual file size
    let data = create_test_elf(
        0x1000,
        0x1000,
        0x200,
        0x10000,  // Much larger than the actual file
    );

    let result = Process::from_elf_data(context, &data as *const _);
    match result {
        Err(ProcessError::InvalidElf(msg)) => {
            assert!(
                msg.contains("file size") || msg.contains("exceeds"),
                "Expected file size error, got: {}",
                msg
            );
        }
        _ => panic!("Expected InvalidElf error for out-of-bounds file read"),
    }
}

/// Test that valid ELF binaries are still accepted
fn test_accept_valid_elf() {
    let context = Context::new();

    // Create a valid ELF with userspace address
    let data = create_test_elf(
        0x1000,      // Valid userspace address
        0x1000,      // Reasonable size
        0x200,       // Valid offset
        0x100,       // Size within file bounds
    );

    let result = Process::from_elf_data(context, &data as *const _);
    assert!(result.is_ok(), "Valid ELF binary should be accepted");
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    eprintln!("{}", info);
    panda_kernel::process::exit(1);
}
