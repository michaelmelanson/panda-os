//! VT-d second-level page tables (SLPT).
//!
//! Four-level structure with the same geometry as x86-64 paging (512 entries,
//! 9 bits per level, 12-bit page offset) but different flag bits: bit 0 = read
//! permission, bit 1 = write permission, no NX bit. This is a distinct format
//! from the kernel's own x86-64 page tables and is implemented independently.

use alloc::collections::BTreeMap;

use crate::hal::FrameAllocator;
use crate::types::{IommuError, IommuFlags};

const ENTRY_READ: u64 = 1 << 0;
const ENTRY_WRITE: u64 = 1 << 1;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

const LEVEL_SHIFTS: [u32; 4] = [39, 30, 21, 12];

fn level_index(iova: u64, level: usize) -> usize {
    ((iova >> LEVEL_SHIFTS[level]) & 0x1FF) as usize
}

fn flags_to_entry_bits(flags: IommuFlags) -> u64 {
    let mut bits = 0;
    if flags.contains(IommuFlags::READ) {
        bits |= ENTRY_READ;
    }
    if flags.contains(IommuFlags::WRITE) {
        bits |= ENTRY_WRITE;
    }
    bits
}

/// One allocated intermediate or leaf-owning table node.
struct SlptNode {
    phys: u64,
    ptr: *mut u64,
}

// Safety: the raw pointer refers to a frame owned exclusively by this node,
// per the `FrameAllocator` contract.
unsafe impl Send for SlptNode {}
unsafe impl Sync for SlptNode {}

impl SlptNode {
    fn alloc<F: FrameAllocator>(alloc: &F) -> Result<Self, IommuError> {
        let (phys, ptr) = alloc.alloc_frame().ok_or(IommuError::OutOfMemory)?;
        unsafe { core::ptr::write_bytes(ptr, 0, 4096) };
        Ok(Self {
            phys,
            ptr: ptr as *mut u64,
        })
    }

    fn entries(&self) -> &[u64] {
        unsafe { core::slice::from_raw_parts(self.ptr, 512) }
    }

    fn entries_mut(&mut self) -> &mut [u64] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr, 512) }
    }

    fn is_empty(&self) -> bool {
        self.entries().iter().all(|&e| e == 0)
    }
}

/// A four-level second-level page table root, owning all of its intermediate
/// nodes. Leaf frames (the physical memory being mapped) are *not* owned here —
/// only the page table structure itself is.
pub struct SlptRoot<F: FrameAllocator> {
    root: SlptNode,
    /// Intermediate (level 1-3) nodes, keyed by the IOVA of their first covered
    /// byte at that level's granularity, so empty nodes can be found and freed.
    nodes: BTreeMap<(usize, u64), SlptNode>,
    allocator: F,
}

impl<F: FrameAllocator> SlptRoot<F> {
    pub fn new(allocator: F) -> Result<Self, IommuError> {
        let root = SlptNode::alloc(&allocator)?;
        Ok(Self {
            root,
            nodes: BTreeMap::new(),
            allocator,
        })
    }

    pub fn root_phys(&self) -> u64 {
        self.root.phys
    }

    /// Map `[iova, iova + size)` to `[phys, phys + size)`.
    /// `size` must be a multiple of 4096 and `iova`/`phys` must be 4 KB aligned.
    ///
    /// Internally this resolves each level-3 (2 MB) leaf table once and fills
    /// every 4 KB entry it covers in a tight loop, rather than re-walking all
    /// four levels per page — mapping large ranges (e.g. the multi-gigabyte
    /// passthrough identity map) would otherwise be dominated by repeated
    /// tree-walk overhead.
    pub fn map(&mut self, iova: u64, phys: u64, size: usize, flags: IommuFlags) -> Result<(), IommuError> {
        if iova % 4096 != 0 || phys % 4096 != 0 || size % 4096 != 0 {
            return Err(IommuError::InvalidMapping);
        }

        const LEAF_TABLE_SPAN: u64 = 1 << LEVEL_SHIFTS[2]; // 2 MB covered by one level-3 table
        let entry_bits = flags_to_entry_bits(flags);

        let end = iova + size as u64;
        let mut cursor = iova;
        let mut phys_cursor = phys;

        while cursor < end {
            let table_base = cursor & !(LEAF_TABLE_SPAN - 1);
            let table_end = table_base + LEAF_TABLE_SPAN;
            let chunk_end = table_end.min(end);

            let leaf_table = self.ensure_level3_table(cursor)?;
            let mut page_iova = cursor;
            let mut page_phys = phys_cursor;
            while page_iova < chunk_end {
                let index = level_index(page_iova, 3);
                leaf_table.entries_mut()[index] = (page_phys & ADDR_MASK) | entry_bits;
                page_iova += 4096;
                page_phys += 4096;
            }

            phys_cursor += chunk_end - cursor;
            cursor = chunk_end;
        }

        Ok(())
    }

    /// Resolve (allocating on demand) the level-3 table covering `iova`,
    /// walking/creating levels 0..2 as needed.
    fn ensure_level3_table(&mut self, iova: u64) -> Result<&mut SlptNode, IommuError> {
        for level in 0..3 {
            let index = level_index(iova, level);
            let key = Self::node_key(level + 1, iova);

            let (child_phys, existed) = if let Some(child) = self.nodes.get(&key) {
                (child.phys, true)
            } else {
                let child = SlptNode::alloc(&self.allocator)?;
                let child_phys = child.phys;
                self.nodes.insert(key, child);
                (child_phys, false)
            };

            if !existed {
                let parent = if level == 0 {
                    &mut self.root
                } else {
                    self.nodes
                        .get_mut(&Self::node_key(level, iova))
                        .expect("parent node must already exist")
                };
                parent.entries_mut()[index] = (child_phys & ADDR_MASK) | ENTRY_READ | ENTRY_WRITE;
            }
        }

        Ok(self
            .nodes
            .get_mut(&Self::node_key(3, iova))
            .expect("level-3 table must exist after the walk above"))
    }

    /// Key an intermediate node at `level` (1..=3) by the IOVA of the first
    /// byte it covers, so a node can be found directly by IOVA in O(log n)
    /// without scanning the whole table for a matching physical address.
    fn node_key(level: usize, iova: u64) -> (usize, u64) {
        (level, iova & !((1u64 << LEVEL_SHIFTS[level - 1]) - 1))
    }

    /// Unmap `[iova, iova + size)`, clearing leaf entries and freeing any
    /// intermediate nodes that become empty as a result.
    pub fn unmap(&mut self, iova: u64, size: usize) {
        if iova % 4096 != 0 || size % 4096 != 0 {
            return;
        }

        let pages = size / 4096;
        for i in 0..pages {
            let page_iova = iova + (i as u64 * 4096);
            self.unmap_one(page_iova);
        }
    }

    fn unmap_one(&mut self, iova: u64) {
        // Verify every intermediate level is actually present before touching
        // anything, using direct key lookups rather than scanning by address.
        for level in 0..3 {
            let index = level_index(iova, level);
            let table_entries = if level == 0 {
                self.root.entries()
            } else {
                match self.nodes.get(&Self::node_key(level, iova)) {
                    Some(n) => n.entries(),
                    None => return,
                }
            };
            if table_entries[index] & ENTRY_READ == 0 {
                // Not mapped at all.
                return;
            }
        }

        // Clear the leaf entry.
        let leaf_index = level_index(iova, 3);
        if let Some(leaf) = self.nodes.get_mut(&Self::node_key(3, iova)) {
            leaf.entries_mut()[leaf_index] = 0;
        }

        // Walk back up, freeing empty intermediate nodes and clearing their
        // parent's entry.
        for level in (0..3).rev() {
            let key = Self::node_key(level + 1, iova);
            let child_empty = self
                .nodes
                .get(&key)
                .map(|n| n.is_empty())
                .unwrap_or(false);

            if !child_empty {
                break;
            }

            // Clear the parent's pointer to this now-empty child.
            let parent_index = level_index(iova, level);
            if level == 0 {
                self.root.entries_mut()[parent_index] = 0;
            } else if let Some(parent) = self.nodes.get_mut(&Self::node_key(level, iova)) {
                parent.entries_mut()[parent_index] = 0;
            }

            if let Some(child) = self.nodes.remove(&key) {
                unsafe {
                    self.allocator.dealloc_frame(child.phys);
                }
            }
        }
    }

    /// Look up the raw leaf entry for `iova` (for testing).
    pub fn leaf_entry(&self, iova: u64) -> Option<u64> {
        for level in 0..3 {
            let index = level_index(iova, level);
            let entries = if level == 0 {
                self.root.entries()
            } else {
                self.nodes.get(&Self::node_key(level, iova))?.entries()
            };
            if entries[index] & ENTRY_READ == 0 {
                return None;
            }
        }
        let node = self.nodes.get(&Self::node_key(3, iova))?;
        let index = level_index(iova, 3);
        let entry = node.entries()[index];
        if entry & ENTRY_READ == 0 && entry & ENTRY_WRITE == 0 {
            None
        } else {
            Some(entry)
        }
    }
}

impl<F: FrameAllocator> Drop for SlptRoot<F> {
    fn drop(&mut self) {
        unsafe {
            self.allocator.dealloc_frame(self.root.phys);
        }
        for (_, node) in self.nodes.iter() {
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
    fn map_single_page_creates_correct_leaf_entry() {
        let mut root = SlptRoot::new(allocator()).unwrap();
        root.map(0x10_0000, 0x20_0000, 4096, IommuFlags::READ_WRITE).unwrap();

        let entry = root.leaf_entry(0x10_0000).unwrap();
        assert_eq!(entry & ADDR_MASK, 0x20_0000);
        assert_eq!(entry & ENTRY_READ, ENTRY_READ);
        assert_eq!(entry & ENTRY_WRITE, ENTRY_WRITE);
    }

    #[test]
    fn map_read_only_clears_write_bit() {
        let mut root = SlptRoot::new(allocator()).unwrap();
        root.map(0, 0x1000, 4096, IommuFlags::READ).unwrap();
        let entry = root.leaf_entry(0).unwrap();
        assert_eq!(entry & ENTRY_READ, ENTRY_READ);
        assert_eq!(entry & ENTRY_WRITE, 0);
    }

    #[test]
    fn map_multi_page_range() {
        let mut root = SlptRoot::new(allocator()).unwrap();
        root.map(0, 0x10_0000, 4 * 4096, IommuFlags::READ_WRITE).unwrap();
        for i in 0..4u64 {
            let entry = root.leaf_entry(i * 4096).unwrap();
            assert_eq!(entry & ADDR_MASK, 0x10_0000 + i * 4096);
        }
    }

    #[test]
    fn unmap_clears_leaf_entry() {
        let mut root = SlptRoot::new(allocator()).unwrap();
        root.map(0x10_0000, 0x20_0000, 4096, IommuFlags::READ_WRITE).unwrap();
        assert!(root.leaf_entry(0x10_0000).is_some());
        root.unmap(0x10_0000, 4096);
        assert!(root.leaf_entry(0x10_0000).is_none());
    }

    #[test]
    fn unmap_frees_empty_intermediate_nodes() {
        let alloc = allocator();
        let allocated_count = || alloc.0.borrow().len();

        let mut root = SlptRoot::new(alloc.clone()).unwrap();
        let before = allocated_count();
        root.map(0x40_0000_0000, 0x1000, 4096, IommuFlags::READ_WRITE).unwrap();
        let after_map = allocated_count();
        assert!(after_map > before, "mapping a distant IOVA should allocate intermediate nodes");

        root.unmap(0x40_0000_0000, 4096);
        let after_unmap = allocated_count();
        assert_eq!(after_unmap, before, "unmapping should free all intermediate nodes it created");
    }

    #[test]
    fn crossing_page_table_boundary_maps_correctly() {
        let mut root = SlptRoot::new(allocator()).unwrap();
        // Map across a level-3 (2 MB) boundary.
        let base = 0x1FF000u64; // last page before 2MB boundary
        root.map(base, 0x9000_0000, 2 * 4096, IommuFlags::READ_WRITE).unwrap();
        assert_eq!(root.leaf_entry(base).unwrap() & ADDR_MASK, 0x9000_0000);
        assert_eq!(root.leaf_entry(base + 4096).unwrap() & ADDR_MASK, 0x9000_0000 + 4096);
    }
}

#[cfg(test)]
mod perf_tests {
    use super::*;
    use alloc::alloc::{alloc as heap_alloc, dealloc as heap_dealloc, Layout};
    use alloc::sync::Arc;
    use core::cell::RefCell;

    #[derive(Clone)]
    struct TestAllocator(Arc<RefCell<alloc::vec::Vec<(u64, *mut u8)>>>);
    unsafe impl Send for TestAllocator {}
    unsafe impl Sync for TestAllocator {}
    unsafe impl FrameAllocator for TestAllocator {
        fn alloc_frame(&self) -> Option<(u64, *mut u8)> {
            let layout = Layout::from_size_align(4096, 4096).unwrap();
            let ptr = unsafe { heap_alloc(layout) };
            if ptr.is_null() { return None; }
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

    #[test]
    fn map_4gb_is_fast() {
        let alloc = TestAllocator(Arc::new(RefCell::new(alloc::vec::Vec::new())));
        let mut root = SlptRoot::new(alloc).unwrap();
        root.map(0, 0, 4usize * 1024 * 1024 * 1024, IommuFlags::READ_WRITE).unwrap();
        assert_eq!(root.leaf_entry(0).unwrap() & ADDR_MASK, 0);
        assert_eq!(root.leaf_entry(0x100000000 - 4096).unwrap() & ADDR_MASK, 0x100000000 - 4096);
    }
}
