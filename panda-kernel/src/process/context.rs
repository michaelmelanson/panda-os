use x86_64::PhysAddr;

use crate::memory;

pub struct Context {
    /// Physical address of this context's PML4 page table.
    page_table_phys: PhysAddr,
}

impl Context {
    /// Create a context that uses the current page table (for init process).
    pub unsafe fn from_current_page_table() -> Self {
        Self {
            page_table_phys: memory::current_page_table_phys(),
        }
    }

    /// Create a new context with a fresh page table for userspace.
    /// Kernel mappings are shared with the current page table.
    pub fn new_user_context() -> Self {
        Self {
            page_table_phys: memory::create_user_page_table(),
        }
    }

    /// Get the physical address of this context's page table.
    pub fn page_table_phys(&self) -> PhysAddr {
        self.page_table_phys
    }

    /// Switch to this context's page table.
    ///
    /// # Safety
    /// Must only be called when it's safe to switch page tables.
    pub unsafe fn activate(&self) {
        unsafe {
            memory::switch_page_table(self.page_table_phys);
        }
    }
}
