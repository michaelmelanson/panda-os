//! VT-d root and context entry tables.
//!
//! The root table is a 4 KB page of 256 root entries (one per PCI bus). Each
//! present root entry points to a 4 KB context table of 256 entries (indexed by
//! `device << 3 | function`). Each context entry records the domain ID and the
//! physical address of that domain's second-level page table root.

use alloc::collections::BTreeMap;

use crate::hal::FrameAllocator;
use crate::types::{DomainId, IommuError};

/// Context entry low qword: present bit.
const CTX_PRESENT: u64 = 1 << 0;
/// Context entry low qword: translation type = 0b00 (untranslated requests use the SLPT).
const CTX_TT_UNTRANSLATED: u64 = 0b00 << 2;
/// Context entry high qword: address width = 0b010 (4-level page table, AGAW = 48-bit),
/// bits 2:0.
const CTX_AW_4_LEVEL: u64 = 0b010;

/// One frame backing a root or context table, owned via the injected allocator.
struct Node<F: FrameAllocator> {
    phys: u64,
    ptr: *mut u64,
    _marker: core::marker::PhantomData<F>,
}

// Safety: the raw pointer refers to a frame owned exclusively by this `Node`,
// per the `FrameAllocator` contract; `F: Send + Sync` covers the allocator.
unsafe impl<F: FrameAllocator> Send for Node<F> {}
unsafe impl<F: FrameAllocator> Sync for Node<F> {}

impl<F: FrameAllocator> Node<F> {
    fn alloc(alloc: &F) -> Result<Self, IommuError> {
        let (phys, ptr) = alloc.alloc_frame().ok_or(IommuError::OutOfMemory)?;
        // Zero the frame so "present" bits default to false everywhere.
        unsafe { core::ptr::write_bytes(ptr, 0, 4096) };
        Ok(Self {
            phys,
            ptr: ptr as *mut u64,
            _marker: core::marker::PhantomData,
        })
    }

    fn entries(&self) -> &[u64] {
        unsafe { core::slice::from_raw_parts(self.ptr, 512) }
    }

    fn entries_mut(&mut self) -> &mut [u64] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr, 512) }
    }
}

/// Root and context table hierarchy for one VT-d unit.
pub struct RootTable<F: FrameAllocator> {
    root: Node<F>,
    /// Context tables, keyed by bus number, allocated lazily.
    context_tables: BTreeMap<u8, Node<F>>,
    allocator: F,
}

impl<F: FrameAllocator> RootTable<F> {
    pub fn new(allocator: F) -> Result<Self, IommuError> {
        let root = Node::alloc(&allocator)?;
        Ok(Self {
            root,
            context_tables: BTreeMap::new(),
            allocator,
        })
    }

    pub fn physical_address(&self) -> u64 {
        self.root.phys
    }

    fn context_table_for_bus(&mut self, bus: u8) -> Result<u64, IommuError> {
        if let Some(node) = self.context_tables.get(&bus) {
            return Ok(node.phys);
        }

        let node = Node::alloc(&self.allocator)?;
        let phys = node.phys;
        self.context_tables.insert(bus, node);

        // Root entry: present bit + context table pointer in bits 12..63.
        // Root entries are indexed by bus number directly, one 64-bit low
        // word (present + pointer) followed by a reserved high word.
        let root_index = bus as usize * 2;
        self.root.entries_mut()[root_index] = CTX_PRESENT | (phys & !0xFFFu64);

        Ok(phys)
    }

    /// Write a context entry for `(bus, device, function)`, pointing at the
    /// given domain's second-level page table root.
    pub fn set_context_entry(
        &mut self,
        bus: u8,
        devfn: u8,
        domain_id: DomainId,
        slpt_root_phys: u64,
    ) -> Result<(), IommuError> {
        let ctx_phys = self.context_table_for_bus(bus)?;
        let ctx_node = self.context_tables.get_mut(&bus).expect("just inserted");
        debug_assert_eq!(ctx_node.phys, ctx_phys);

        let index = devfn as usize * 2;
        // Low qword: present, translation type, address width, SLPT root pointer.
        let low = CTX_PRESENT | CTX_TT_UNTRANSLATED | (slpt_root_phys & !0xFFFu64);
        // High qword: address width in bits 2:0, domain ID in bits 8..23.
        let high = CTX_AW_4_LEVEL | ((domain_id as u64 & 0xFFFF) << 8);

        ctx_node.entries_mut()[index] = low;
        ctx_node.entries_mut()[index + 1] = high;

        Ok(())
    }

    /// Read back a raw context entry (for testing/round-trip verification).
    pub fn read_context_entry(&self, bus: u8, devfn: u8) -> Option<(u64, u64)> {
        let node = self.context_tables.get(&bus)?;
        let index = devfn as usize * 2;
        let entries = node.entries();
        Some((entries[index], entries[index + 1]))
    }
}

impl<F: FrameAllocator> Drop for RootTable<F> {
    fn drop(&mut self) {
        unsafe {
            self.allocator.dealloc_frame(self.root.phys);
        }
        for (_, node) in self.context_tables.iter() {
            unsafe {
                self.allocator.dealloc_frame(node.phys);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::alloc::{alloc as heap_alloc, dealloc as heap_dealloc, Layout};
    use alloc::sync::Arc;
    use core::cell::RefCell;

    /// A host-testable frame allocator backed by the global heap.
    #[derive(Clone)]
    struct TestAllocator(Arc<RefCell<alloc::vec::Vec<(u64, *mut u8)>>>);

    unsafe impl Send for TestAllocator {}
    unsafe impl Sync for TestAllocator {}

    unsafe impl FrameAllocator for TestAllocator {
        fn alloc_frame(&self) -> Option<(u64, *mut u8)> {
            let layout = Layout::from_size_align(4096, 4096).unwrap();
            let ptr = unsafe { heap_alloc(layout) };
            if ptr.is_null() {
                return None;
            }
            let phys = ptr as u64;
            self.0.borrow_mut().push((phys, ptr));
            Some((phys, ptr))
        }

        unsafe fn dealloc_frame(&self, phys: u64) {
            let mut frames = self.0.borrow_mut();
            if let Some(idx) = frames.iter().position(|&(p, _)| p == phys) {
                let (_, ptr) = frames.remove(idx);
                let layout = Layout::from_size_align(4096, 4096).unwrap();
                unsafe { heap_dealloc(ptr, layout) };
            }
        }
    }

    fn allocator() -> TestAllocator {
        TestAllocator(Arc::new(RefCell::new(alloc::vec::Vec::new())))
    }

    #[test]
    fn context_entry_round_trip() {
        let mut table = RootTable::new(allocator()).unwrap();
        let slpt_phys = 0x1234_5000u64;
        let domain_id: DomainId = 7;
        table.set_context_entry(0x02, 0x00, domain_id, slpt_phys).unwrap();

        let (low, high) = table.read_context_entry(0x02, 0x00).unwrap();
        assert_eq!(low & CTX_PRESENT, CTX_PRESENT);
        assert_eq!(low & !0xFFFu64, slpt_phys & !0xFFFu64);
        assert_eq!((high >> 8) & 0xFFFF, domain_id as u64);
        assert_eq!(high & 0b111, CTX_AW_4_LEVEL);
    }

    #[test]
    fn distinct_devfns_get_distinct_entries() {
        let mut table = RootTable::new(allocator()).unwrap();
        table.set_context_entry(0, 0x10, 1, 0x1000).unwrap();
        table.set_context_entry(0, 0x18, 2, 0x2000).unwrap();

        let (low1, high1) = table.read_context_entry(0, 0x10).unwrap();
        let (low2, high2) = table.read_context_entry(0, 0x18).unwrap();
        assert_ne!(low1, low2);
        assert_eq!((high1 >> 8) & 0xFFFF, 1);
        assert_eq!((high2 >> 8) & 0xFFFF, 2);
    }
}
