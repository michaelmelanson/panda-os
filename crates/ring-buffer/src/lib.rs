//! A fixed-capacity ring buffer backed by a pre-allocated `Vec`.
//!
//! Designed for `no_std` + `alloc` environments where eliminating per-operation
//! heap allocations is critical. When the buffer is full, new entries overwrite
//! the oldest, and callers can recycle the evicted slot's allocations instead of
//! dropping and reallocating.

#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

/// A fixed-capacity circular buffer.
///
/// Once full, pushing a new item overwrites the oldest entry. The buffer
/// provides index-based access where index `0` is the oldest live entry
/// and index `len() - 1` is the newest.
pub struct RingBuffer<T> {
    /// Backing storage, allocated once to `capacity` during construction.
    buf: Vec<T>,
    /// Index into `buf` where the next write will go.
    head: usize,
    /// Number of live entries (capped at `buf.capacity()`).
    len: usize,
}

impl<T> RingBuffer<T> {
    /// Create a new ring buffer with the given capacity.
    ///
    /// The backing `Vec` is allocated once and never reallocated.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "RingBuffer capacity must be > 0");
        Self {
            buf: Vec::with_capacity(capacity),
            head: 0,
            len: 0,
        }
    }

    /// The maximum number of entries this buffer can hold.
    pub fn capacity(&self) -> usize {
        self.buf.capacity()
    }

    /// The number of live entries currently in the buffer.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether the buffer is at capacity.
    pub fn is_full(&self) -> bool {
        self.len == self.buf.capacity()
    }

    /// Push an item, overwriting the oldest entry if full.
    ///
    /// Returns `None` if the buffer was not full (no eviction), or
    /// `Some(old)` with the evicted value if it was full.
    pub fn push(&mut self, item: T) -> Option<T> {
        let cap = self.buf.capacity();
        if self.buf.len() < cap {
            // Still filling the initial allocation.
            self.buf.push(item);
            self.head = self.buf.len() % cap;
            self.len = self.buf.len();
            None
        } else {
            // Buffer full — overwrite oldest slot.
            let old = core::mem::replace(&mut self.buf[self.head], item);
            self.head = (self.head + 1) % cap;
            Some(old)
        }
    }

    /// Get a mutable reference to the slot that *would* be overwritten by
    /// the next [`push`](Self::push), without actually writing anything.
    ///
    /// Returns `None` if the buffer is not yet full (no slot to recycle).
    ///
    /// This is the key API for allocation-free recycling: the caller can
    /// clear the old value's internal allocations (e.g., `Vec::clear()`,
    /// `String::clear()`) and then repurpose the slot, avoiding a drop +
    /// reallocation cycle.
    ///
    /// After recycling, call [`advance_head`](Self::advance_head) to
    /// commit the recycle and move the write cursor forward.
    pub fn next_evictable(&mut self) -> Option<&mut T> {
        if self.is_full() {
            Some(&mut self.buf[self.head])
        } else {
            None
        }
    }

    /// Advance the write cursor after a [`next_evictable`](Self::next_evictable)
    /// recycle. This must only be called when the buffer is full.
    ///
    /// # Panics
    ///
    /// Panics if the buffer is not full.
    pub fn advance_head(&mut self) {
        assert!(self.is_full(), "advance_head called on a non-full buffer");
        self.head = (self.head + 1) % self.buf.capacity();
    }

    /// Push a new item by recycling: if the buffer is full, the closure
    /// receives a mutable reference to the oldest slot so it can be cleared
    /// and reused. If the buffer is not full, the closure receives `None`
    /// and the caller must return a freshly created item.
    ///
    /// This is a convenience wrapper around [`next_evictable`](Self::next_evictable)
    /// + [`advance_head`](Self::advance_head) / [`push`](Self::push).
    pub fn push_or_recycle<F>(&mut self, recycle: F)
    where
        F: FnOnce(Option<&mut T>) -> Option<T>,
    {
        if self.is_full() {
            let slot = &mut self.buf[self.head];
            // The closure gets the old slot to clear/reuse.
            // It should return None to indicate it recycled in-place.
            let result = recycle(Some(slot));
            if let Some(new_item) = result {
                // Caller chose not to recycle — replace entirely.
                self.buf[self.head] = new_item;
            }
            self.head = (self.head + 1) % self.buf.capacity();
        } else {
            // Not full — need a brand new item.
            if let Some(item) = recycle(None) {
                self.buf.push(item);
                let cap = self.buf.capacity();
                self.head = self.buf.len() % cap;
                self.len = self.buf.len();
            }
        }
    }

    /// Access an element by logical index, where `0` is the oldest live
    /// entry and `len() - 1` is the newest.
    ///
    /// Returns `None` if `index >= len()`.
    pub fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None;
        }
        let cap = self.buf.capacity();
        let actual = if self.is_full() {
            (self.head + index) % cap
        } else {
            index
        };
        Some(&self.buf[actual])
    }

    /// Mutable access by logical index.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        if index >= self.len {
            return None;
        }
        let cap = self.buf.capacity();
        let actual = if self.is_full() {
            (self.head + index) % cap
        } else {
            index
        };
        Some(&mut self.buf[actual])
    }

    /// Get a reference to the most recently pushed element.
    pub fn last(&self) -> Option<&T> {
        if self.len == 0 {
            return None;
        }
        self.get(self.len - 1)
    }

    /// Get a mutable reference to the most recently pushed element.
    pub fn last_mut(&mut self) -> Option<&mut T> {
        if self.len == 0 {
            return None;
        }
        let idx = self.len - 1;
        self.get_mut(idx)
    }

    /// Iterate over all live entries from oldest to newest.
    pub fn iter(&self) -> RingBufferIter<'_, T> {
        RingBufferIter {
            buf: self,
            index: 0,
        }
    }

    /// Clear all entries, resetting length to zero.
    ///
    /// Note: this drops all stored values but does not deallocate the
    /// backing storage.
    pub fn clear(&mut self) {
        self.buf.clear();
        self.head = 0;
        self.len = 0;
    }
}

/// Iterator over ring buffer entries from oldest to newest.
pub struct RingBufferIter<'a, T> {
    buf: &'a RingBuffer<T>,
    index: usize,
}

impl<'a, T> Iterator for RingBufferIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.buf.get(self.index)?;
        self.index += 1;
        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.buf.len() - self.index;
        (remaining, Some(remaining))
    }
}

impl<'a, T> ExactSizeIterator for RingBufferIter<'a, T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_buffer_is_empty() {
        let rb: RingBuffer<i32> = RingBuffer::new(5);
        assert!(rb.is_empty());
        assert_eq!(rb.len(), 0);
        assert_eq!(rb.capacity(), 5);
    }

    #[test]
    fn test_push_and_get() {
        let mut rb = RingBuffer::new(3);
        assert!(rb.push(10).is_none());
        assert!(rb.push(20).is_none());
        assert!(rb.push(30).is_none());

        assert_eq!(rb.len(), 3);
        assert!(rb.is_full());
        assert_eq!(rb.get(0), Some(&10));
        assert_eq!(rb.get(1), Some(&20));
        assert_eq!(rb.get(2), Some(&30));
    }

    #[test]
    fn test_wrapping() {
        let mut rb = RingBuffer::new(3);
        rb.push(1);
        rb.push(2);
        rb.push(3);

        // Overwrites oldest (1)
        let evicted = rb.push(4);
        assert_eq!(evicted, Some(1));
        assert_eq!(rb.get(0), Some(&2));
        assert_eq!(rb.get(1), Some(&3));
        assert_eq!(rb.get(2), Some(&4));

        // Overwrites oldest (2)
        let evicted = rb.push(5);
        assert_eq!(evicted, Some(2));
        assert_eq!(rb.get(0), Some(&3));
        assert_eq!(rb.get(1), Some(&4));
        assert_eq!(rb.get(2), Some(&5));
    }

    #[test]
    fn test_last() {
        let mut rb = RingBuffer::new(3);
        assert_eq!(rb.last(), None);

        rb.push(1);
        assert_eq!(rb.last(), Some(&1));

        rb.push(2);
        rb.push(3);
        assert_eq!(rb.last(), Some(&3));

        rb.push(4); // wraps
        assert_eq!(rb.last(), Some(&4));
    }

    #[test]
    fn test_last_mut() {
        let mut rb = RingBuffer::new(3);
        rb.push(1);
        *rb.last_mut().unwrap() = 100;
        assert_eq!(rb.get(0), Some(&100));
    }

    #[test]
    fn test_iter() {
        let mut rb = RingBuffer::new(3);
        rb.push(1);
        rb.push(2);
        rb.push(3);
        rb.push(4); // evicts 1

        let items: Vec<&i32> = rb.iter().collect();
        assert_eq!(items, vec![&2, &3, &4]);
    }

    #[test]
    fn test_iter_len() {
        let mut rb = RingBuffer::new(5);
        rb.push(1);
        rb.push(2);
        rb.push(3);

        let iter = rb.iter();
        assert_eq!(iter.len(), 3);
    }

    #[test]
    fn test_clear() {
        let mut rb = RingBuffer::new(3);
        rb.push(1);
        rb.push(2);
        rb.push(3);
        rb.clear();

        assert!(rb.is_empty());
        assert_eq!(rb.len(), 0);
        assert_eq!(rb.capacity(), 3);
    }

    #[test]
    fn test_out_of_bounds() {
        let mut rb = RingBuffer::new(3);
        rb.push(1);
        assert_eq!(rb.get(0), Some(&1));
        assert_eq!(rb.get(1), None);
        assert_eq!(rb.get(100), None);
    }

    #[test]
    fn test_next_evictable() {
        let mut rb = RingBuffer::new(2);
        assert!(rb.next_evictable().is_none()); // not full

        rb.push(10);
        assert!(rb.next_evictable().is_none()); // still not full

        rb.push(20);
        // Now full — oldest is 10
        assert_eq!(rb.next_evictable(), Some(&mut 10));
    }

    #[test]
    fn test_push_or_recycle() {
        let mut rb = RingBuffer::new(2);

        // Not full — closure gets None, must return Some(new item)
        rb.push_or_recycle(|slot| {
            assert!(slot.is_none());
            Some(100)
        });
        rb.push_or_recycle(|slot| {
            assert!(slot.is_none());
            Some(200)
        });

        assert_eq!(rb.get(0), Some(&100));
        assert_eq!(rb.get(1), Some(&200));

        // Full — closure gets oldest slot for recycling
        rb.push_or_recycle(|slot| {
            let slot = slot.unwrap();
            assert_eq!(*slot, 100);
            *slot = 300; // recycle in-place
            None // signal recycled
        });

        assert_eq!(rb.get(0), Some(&200));
        assert_eq!(rb.get(1), Some(&300));
    }

    #[test]
    fn test_many_wraps() {
        let mut rb = RingBuffer::new(3);
        for i in 0..100 {
            rb.push(i);
        }
        // Should contain last 3 values: 97, 98, 99
        assert_eq!(rb.get(0), Some(&97));
        assert_eq!(rb.get(1), Some(&98));
        assert_eq!(rb.get(2), Some(&99));
        assert_eq!(rb.len(), 3);
    }

    #[test]
    #[should_panic(expected = "capacity must be > 0")]
    fn test_zero_capacity_panics() {
        let _: RingBuffer<i32> = RingBuffer::new(0);
    }
}
