//! ELF binary loading.

use alloc::vec::Vec;

use goblin::elf::{
    Elf,
    program_header::{PT_LOAD, pt_to_str},
};
use log::trace;
use x86_64::VirtAddr;

use crate::memory::{self, Mapping, MemoryMappingOptions};

/// Load an ELF binary into the current address space.
/// Returns the list of mappings created.
pub fn load_elf(elf: &Elf<'_>, file_ptr: *const [u8]) -> Vec<Mapping> {
    let mut mappings = Vec::new();

    for header in &elf.program_headers {
        match header.p_type {
            PT_LOAD => {
                let virt_addr = VirtAddr::new(header.p_vaddr);

                // Align down to page boundary for mapping
                let page_offset = virt_addr.as_u64() & 0xFFF;
                let aligned_virt_addr = virt_addr.align_down(4096u64);
                let aligned_size = header.p_memsz as usize + page_offset as usize;

                let mapping = memory::allocate_and_map(
                    aligned_virt_addr,
                    aligned_size,
                    MemoryMappingOptions {
                        user: true,
                        executable: header.is_executable(),
                        writable: header.is_write(),
                    },
                );

                // Copy ELF data to the mapped region (at the original unaligned address)
                // Temporarily disable write protection to allow kernel writes to read-only pages
                let src_ptr = unsafe { file_ptr.byte_add(header.p_offset as usize) as *const u8 };
                memory::without_write_protection(|| unsafe {
                    core::ptr::copy_nonoverlapping(
                        src_ptr,
                        virt_addr.as_mut_ptr(),
                        header.p_filesz as usize,
                    );
                });

                mappings.push(mapping);
            }

            _ => trace!("Ignoring {} program header", pt_to_str(header.p_type)),
        }
    }

    mappings
}
