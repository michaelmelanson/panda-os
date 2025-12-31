use core::{alloc::Layout, arch::asm};
use goblin::elf::{
    Elf,
    program_header::{PT_LOAD, pt_to_str},
};
use log::{debug, info, trace};
use x86_64::{VirtAddr, registers::rflags::RFlags};

use crate::memory::{self, MemoryMappingOptions, allocate_frame, allocate_physical};

pub fn exec_raw(ptr: *const [u8]) -> ! {
    let data = unsafe { ptr.as_ref().unwrap() };

    info!("Exec of program at {ptr:?}");

    let elf = Elf::parse(data).expect("failed to parse ELF binary");
    assert_eq!(elf.is_64, true, "32-bit binaries are not supported");

    load_elf(&elf, ptr);

    let stack_frame = allocate_frame();
    let stack_base = VirtAddr::new(0xb0000000000);
    let stack_pointer = stack_base + stack_frame.size() - 4;
    memory::map(
        stack_frame.start_address(),
        stack_base,
        stack_frame.size() as usize,
        MemoryMappingOptions {
            user: true,
            executable: false,
            writable: true,
        },
    );

    let rflags = RFlags::INTERRUPT_FLAG;
    info!(
        "Jumping to start address {:#0X} (RFLAGS: {rflags:?} / {:#X})",
        elf.entry,
        rflags.bits()
    );

    let rflags = rflags.bits();

    unsafe {
        asm!("int3");

        asm!(
            "mov rsp, {stack_pointer}",
            "sysretq",
            in("ecx") elf.entry,
            in("r11") rflags,
            stack_pointer = in(reg) stack_pointer.as_u64()
        );
    }
    info!("RETURNED");

    loop {}
}

fn load_elf(elf: &Elf<'_>, file_ptr: *const [u8]) {
    for header in &elf.program_headers {
        match header.p_type {
            PT_LOAD => {
                let phys_addr = allocate_physical(
                    Layout::from_size_align(header.p_memsz as usize, 4096).unwrap(),
                );
                let region_ptr = unsafe { file_ptr.byte_add(header.p_offset as usize) as *mut u8 };
                unsafe {
                    region_ptr.copy_to_nonoverlapping(
                        phys_addr.start_address().as_u64() as *mut u8,
                        header.p_memsz as usize,
                    );
                }

                debug!(
                    "Load: {:#0X} to {:#0X}",
                    phys_addr.start_address(),
                    header.p_vaddr
                );

                let virt_addr = VirtAddr::new(header.p_vaddr);

                memory::map(
                    phys_addr.start_address(),
                    virt_addr,
                    header.p_memsz as usize,
                    MemoryMappingOptions {
                        user: true,
                        executable: header.is_executable(),
                        writable: header.is_write(),
                    },
                );

                // debug!("Verifying mapping...");
                // unsafe {
                //     for i in 0..4 {
                //         let src = virt_addr.as_ptr::<u8>().byte_add(i).read();
                //         let dest = (file_ptr as *const u8)
                //             .byte_add(header.p_offset as usize + i)
                //             .read();
                //         debug!("after mapping: index={i}, src={src:02X}, dest={dest:02X}");
                //         assert_eq!(src, dest, "mapping failed");
                //     }
                // }
            }

            _ => trace!("Ignoring {} program header", pt_to_str(header.p_type)),
        }
    }
}
