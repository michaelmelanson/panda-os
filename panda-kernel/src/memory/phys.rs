//! Physical memory access with RAII wrappers.
//!
//! This module provides type-safe access to physical memory through RAII wrappers
//! that ensure proper virtual address translation.
//!
//! These wrappers store the physical address and compute the virtual address on
//! each access, ensuring they remain valid even if `PHYS_MAP_BASE` changes.

use core::marker::PhantomData;
use core::ptr::{read_volatile, write_volatile};

use x86_64::{PhysAddr, VirtAddr};

use super::address::physical_address_to_virtual;

/// RAII wrapper for accessing physical memory as a typed reference.
///
/// This provides safe, typed access to physical memory by translating the physical
/// address to a virtual address using the current physical memory mapping strategy
/// (identity mapping initially, physical window later).
///
/// The physical address is stored and translated on each access, ensuring the
/// mapping remains valid even if `PHYS_MAP_BASE` changes.
///
/// # Example
///
/// ```ignore
/// let mapping = PhysicalMapping::<PageTable>::new(page_table_phys_addr);
/// let table = mapping.as_ref();
/// ```
pub struct PhysicalMapping<T> {
    phys_addr: PhysAddr,
    _marker: PhantomData<T>,
}

impl<T> PhysicalMapping<T> {
    /// Create a new physical mapping for the given physical address.
    ///
    /// The physical address must be properly aligned for type T.
    pub fn new(phys_addr: PhysAddr) -> Self {
        debug_assert!(
            phys_addr.as_u64() % core::mem::align_of::<T>() as u64 == 0,
            "physical address not aligned for type"
        );
        Self {
            phys_addr,
            _marker: PhantomData,
        }
    }

    /// Get the physical address of the mapping.
    pub fn phys_addr(&self) -> PhysAddr {
        self.phys_addr
    }

    /// Get the current virtual address of the mapping.
    ///
    /// This is computed from the physical address using the current `PHYS_MAP_BASE`.
    pub fn virt_addr(&self) -> VirtAddr {
        physical_address_to_virtual(self.phys_addr)
    }

    /// Get an immutable reference to the mapped value.
    ///
    /// # Safety
    ///
    /// The caller must ensure:
    /// - The physical memory contains a valid instance of T
    /// - No mutable references to the same memory exist
    pub unsafe fn as_ref(&self) -> &T {
        unsafe { &*self.virt_addr().as_ptr() }
    }

    /// Get a mutable reference to the mapped value.
    ///
    /// # Safety
    ///
    /// The caller must ensure:
    /// - The physical memory contains a valid instance of T
    /// - No other references to the same memory exist
    pub unsafe fn as_mut(&mut self) -> &mut T {
        unsafe { &mut *self.virt_addr().as_mut_ptr() }
    }

    /// Read the value using volatile semantics.
    ///
    /// Use this for memory that may be modified by hardware or other processors.
    ///
    /// # Safety
    ///
    /// The caller must ensure the physical memory contains a valid instance of T.
    pub unsafe fn read_volatile(&self) -> T
    where
        T: Copy,
    {
        unsafe { read_volatile(self.virt_addr().as_ptr()) }
    }

    /// Write a value using volatile semantics.
    ///
    /// Use this for memory that may be read by hardware or other processors.
    ///
    /// # Safety
    ///
    /// The caller must ensure writing to this physical memory is valid.
    pub unsafe fn write_volatile(&mut self, value: T)
    where
        T: Copy,
    {
        unsafe { write_volatile(self.virt_addr().as_mut_ptr(), value) }
    }
}

/// RAII wrapper for accessing a slice of physical memory.
///
/// Similar to `PhysicalMapping<T>` but for contiguous arrays of values.
///
/// The physical address is stored and translated on each access, ensuring the
/// mapping remains valid even if `PHYS_MAP_BASE` changes.
pub struct PhysicalSlice<T> {
    phys_addr: PhysAddr,
    len: usize,
    _marker: PhantomData<T>,
}

impl<T> PhysicalSlice<T> {
    /// Create a new physical slice mapping.
    ///
    /// # Arguments
    ///
    /// * `phys_addr` - Physical address of the start of the slice
    /// * `len` - Number of elements in the slice
    pub fn new(phys_addr: PhysAddr, len: usize) -> Self {
        debug_assert!(
            phys_addr.as_u64() % core::mem::align_of::<T>() as u64 == 0,
            "physical address not aligned for type"
        );
        Self {
            phys_addr,
            len,
            _marker: PhantomData,
        }
    }

    /// Get the physical address of the slice.
    pub fn phys_addr(&self) -> PhysAddr {
        self.phys_addr
    }

    /// Get the current virtual address of the slice.
    ///
    /// This is computed from the physical address using the current `PHYS_MAP_BASE`.
    pub fn virt_addr(&self) -> VirtAddr {
        physical_address_to_virtual(self.phys_addr)
    }

    /// Get the length of the slice.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Check if the slice is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Get an immutable slice reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure:
    /// - The physical memory contains valid instances of T
    /// - No mutable references to the same memory exist
    pub unsafe fn as_slice(&self) -> &[T] {
        unsafe { core::slice::from_raw_parts(self.virt_addr().as_ptr(), self.len) }
    }

    /// Get a mutable slice reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure:
    /// - The physical memory contains valid instances of T
    /// - No other references to the same memory exist
    pub unsafe fn as_mut_slice(&mut self) -> &mut [T] {
        unsafe { core::slice::from_raw_parts_mut(self.virt_addr().as_mut_ptr(), self.len) }
    }
}
