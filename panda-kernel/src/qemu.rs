use x86_64::instructions::port::Port;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) -> ! {
    unsafe {
        // Disable interrupts before exiting
        x86_64::instructions::interrupts::disable();
        // isa-debug-exit configured at iobase=0xf4, iosize=0x04
        let mut port: Port<u32> = Port::new(0xf4);
        port.write(exit_code as u32);
    }
    loop {
        x86_64::instructions::hlt();
    }
}
