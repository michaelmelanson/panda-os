use x86_64::{VirtAddr, registers::control::Cr3};

use crate::memory::physical_address_to_virtual;

pub struct Context {
    page_table_ptr: VirtAddr,
}

fn current_page_table_ptr() -> VirtAddr {
    let (frame, _) = Cr3::read();
    physical_address_to_virtual(frame.start_address())
}

impl Context {
    pub unsafe fn from_current_page_table() -> Self {
        Self {
            page_table_ptr: current_page_table_ptr(),
        }
    }
}
