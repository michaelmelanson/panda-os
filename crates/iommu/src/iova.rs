//! Free-list allocator over an I/O virtual address range.

use alloc::vec::Vec;

/// A free-list allocator for I/O virtual addresses within a fixed range.
///
/// Free regions are kept sorted by base address as `(base, size)` pairs and
/// coalesced with their neighbours on free.
pub struct IovaAllocator {
    /// Sorted, non-overlapping free regions.
    free: Vec<(u64, usize)>,
}

impl IovaAllocator {
    /// Create an allocator covering `[start, end)`.
    pub fn new(start: u64, end: u64) -> Self {
        assert!(end > start, "IovaAllocator range must be non-empty");
        Self {
            free: alloc::vec![(start, (end - start) as usize)],
        }
    }

    /// Allocate `size` bytes aligned to `align` (must be a power of two).
    pub fn alloc(&mut self, size: usize, align: u64) -> Option<u64> {
        if size == 0 {
            return None;
        }
        assert!(align.is_power_of_two(), "alignment must be a power of two");

        for i in 0..self.free.len() {
            let (base, region_size) = self.free[i];
            let aligned_base = (base + align - 1) & !(align - 1);
            let padding = (aligned_base - base) as usize;
            if padding.checked_add(size)? > region_size {
                continue;
            }

            self.free.remove(i);

            // Return leading padding to the free list.
            if padding > 0 {
                self.free.insert(i, (base, padding));
            }

            // Return trailing space to the free list.
            let used_end = aligned_base + size as u64;
            let region_end = base + region_size as u64;
            if used_end < region_end {
                let insert_at = if padding > 0 { i + 1 } else { i };
                self.free
                    .insert(insert_at, (used_end, (region_end - used_end) as usize));
            }

            return Some(aligned_base);
        }

        None
    }

    /// Return a previously-allocated range to the free list, coalescing with
    /// adjacent free regions.
    pub fn free(&mut self, base: u64, size: usize) {
        if size == 0 {
            return;
        }

        // Find insertion point (first region whose base is >= `base`).
        let idx = self.free.partition_point(|&(b, _)| b < base);

        let mut new_base = base;
        let mut new_size = size;

        // Merge with predecessor if adjacent.
        let mut insert_at = idx;
        if idx > 0 {
            let (pred_base, pred_size) = self.free[idx - 1];
            if pred_base + pred_size as u64 == new_base {
                new_base = pred_base;
                new_size += pred_size;
                self.free.remove(idx - 1);
                insert_at = idx - 1;
            }
        }

        self.free.insert(insert_at, (new_base, new_size));
        self.merge_successor(insert_at, new_base, new_size);
    }

    /// After inserting `(new_base, new_size)` at `idx`, merge with the following
    /// region if it is adjacent.
    fn merge_successor(&mut self, idx: usize, new_base: u64, new_size: usize) {
        if let Some(&(succ_base, succ_size)) = self.free.get(idx + 1) {
            if new_base + new_size as u64 == succ_base {
                self.free[idx] = (new_base, new_size + succ_size);
                self.free.remove(idx + 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_and_free_round_trip() {
        let mut a = IovaAllocator::new(0x1000, 0x10000);
        let addr = a.alloc(0x1000, 0x1000).expect("alloc failed");
        assert_eq!(addr, 0x1000);
        a.free(addr, 0x1000);
        // Freeing should coalesce back into a single full-range region.
        assert_eq!(a.free, alloc::vec![(0x1000, 0xF000)]);
    }

    #[test]
    fn sequential_allocations_do_not_overlap() {
        let mut a = IovaAllocator::new(0, 0x4000);
        let a1 = a.alloc(0x1000, 0x1000).unwrap();
        let a2 = a.alloc(0x1000, 0x1000).unwrap();
        let a3 = a.alloc(0x1000, 0x1000).unwrap();
        assert_eq!(a1, 0);
        assert_eq!(a2, 0x1000);
        assert_eq!(a3, 0x2000);
    }

    #[test]
    fn alignment_is_respected() {
        let mut a = IovaAllocator::new(0x100, 0x10000);
        // First carve out a small unaligned chunk to force padding on the next alloc.
        let small = a.alloc(0x10, 1).unwrap();
        assert_eq!(small, 0x100);
        let aligned = a.alloc(0x1000, 0x1000).unwrap();
        assert_eq!(aligned % 0x1000, 0);
        assert!(aligned >= 0x110);
    }

    #[test]
    fn coalescing_merges_adjacent_free_regions() {
        let mut a = IovaAllocator::new(0, 0x3000);
        let a1 = a.alloc(0x1000, 0x1000).unwrap();
        let a2 = a.alloc(0x1000, 0x1000).unwrap();
        let a3 = a.alloc(0x1000, 0x1000).unwrap();

        // Free out of order; after all three frees, the whole range should coalesce.
        a.free(a2, 0x1000);
        a.free(a1, 0x1000);
        a.free(a3, 0x1000);

        assert_eq!(a.free, alloc::vec![(0, 0x3000)]);
    }

    #[test]
    fn out_of_space_returns_none() {
        let mut a = IovaAllocator::new(0, 0x1000);
        assert!(a.alloc(0x1001, 1).is_none());
        let _ = a.alloc(0x1000, 1).unwrap();
        assert!(a.alloc(1, 1).is_none());
    }
}
