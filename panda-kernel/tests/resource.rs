//! Tests for the Resource trait and interface traits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;

use panda_kernel::handle::HandleTable;
use panda_kernel::resource::{
    CharOutError, CharacterOutput, DirEntry, DirectoryResource, Resource,
};

panda_kernel::test_harness!(
    directory_interface_available,
    directory_entry_access,
    directory_count,
    directory_entry_past_end,
    directory_empty,
    mock_char_output,
    resource_interface_dispatch,
    handle_table_insert_and_get,
    handle_table_remove,
    handle_table_offset_state,
    buffer_free_list_no_merge,
    buffer_free_list_merge_with_next,
    buffer_free_list_merge_with_prev,
    buffer_free_list_merge_both,
    buffer_free_list_partial_reuse,
    buffer_free_list_multiple_free_ranges,
);

// =============================================================================
// Mock CharacterOutput resource for testing
// =============================================================================

use spinning_top::Spinlock;

/// A mock character output that records what was written.
struct MockCharOutput {
    written: Spinlock<alloc::vec::Vec<u8>>,
}

impl MockCharOutput {
    fn new() -> Self {
        Self {
            written: Spinlock::new(alloc::vec::Vec::new()),
        }
    }

    fn get_written(&self) -> alloc::vec::Vec<u8> {
        self.written.lock().clone()
    }
}

impl Resource for MockCharOutput {
    fn handle_type(&self) -> panda_abi::HandleType {
        panda_abi::HandleType::File
    }

    fn as_char_output(&self) -> Option<&dyn CharacterOutput> {
        Some(self)
    }
}

impl CharacterOutput for MockCharOutput {
    fn write(&self, buf: &[u8]) -> Result<usize, CharOutError> {
        self.written.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
}

// =============================================================================
// Test fixtures
// =============================================================================

/// Create a test directory resource
fn create_test_directory() -> Arc<dyn Resource> {
    let entries = vec![
        DirEntry {
            name: String::from("file1.txt"),
            is_dir: false,
        },
        DirEntry {
            name: String::from("subdir"),
            is_dir: true,
        },
        DirEntry {
            name: String::from("file2.txt"),
            is_dir: false,
        },
    ];
    Arc::new(DirectoryResource::new(entries))
}

// =============================================================================
// Directory interface tests
// =============================================================================

fn directory_interface_available() {
    let resource = create_test_directory();
    assert!(
        resource.as_directory().is_some(),
        "Directory resource should implement Directory"
    );
    assert!(
        resource.as_event_source().is_none(),
        "Directory resource should not implement EventSource"
    );
}

fn directory_entry_access() {
    let resource = create_test_directory();
    let directory = resource.as_directory().unwrap();

    let entry0 = directory.entry(0).expect("Should have entry 0");
    assert_eq!(entry0.name, "file1.txt");
    assert!(!entry0.is_dir);

    let entry1 = directory.entry(1).expect("Should have entry 1");
    assert_eq!(entry1.name, "subdir");
    assert!(entry1.is_dir);

    let entry2 = directory.entry(2).expect("Should have entry 2");
    assert_eq!(entry2.name, "file2.txt");
    assert!(!entry2.is_dir);
}

fn directory_count() {
    let resource = create_test_directory();
    let directory = resource.as_directory().unwrap();

    assert_eq!(directory.count(), 3, "Directory should have 3 entries");
}

fn directory_entry_past_end() {
    let resource = create_test_directory();
    let directory = resource.as_directory().unwrap();

    let entry3 = directory.entry(3);
    assert!(entry3.is_none(), "Entry 3 should not exist");

    let entry100 = directory.entry(100);
    assert!(entry100.is_none(), "Entry 100 should not exist");
}

// =============================================================================
// Resource dispatch tests
// =============================================================================

fn resource_interface_dispatch() {
    let dir = create_test_directory();

    // Directory: has Directory
    assert!(dir.as_directory().is_some());

    // Can be treated as dyn Resource
    fn check_resource(_r: &dyn Resource) {}
    check_resource(dir.as_ref());
}

// =============================================================================
// Additional directory tests
// =============================================================================

fn directory_empty() {
    let entries = vec![];
    let resource: Box<dyn Resource> = Box::new(DirectoryResource::new(entries));
    let directory = resource.as_directory().unwrap();

    assert_eq!(
        directory.count(),
        0,
        "Empty directory should have 0 entries"
    );
    assert!(
        directory.entry(0).is_none(),
        "Empty directory should have no entries"
    );
}

// =============================================================================
// CharacterOutput tests
// =============================================================================

fn mock_char_output() {
    let output = MockCharOutput::new();

    // Verify interface dispatch
    assert!(output.as_char_output().is_some());
    assert!(output.as_directory().is_none());

    // Write some data
    let char_out = output.as_char_output().unwrap();
    let n = char_out.write(b"Hello").expect("Write should succeed");
    assert_eq!(n, 5);

    let n = char_out.write(b" World").expect("Write should succeed");
    assert_eq!(n, 6);

    // Verify written data
    assert_eq!(output.get_written(), b"Hello World");
}

// =============================================================================
// Handle table tests
// =============================================================================

fn handle_table_insert_and_get() {
    let mut table = HandleTable::new();

    let resource1 = create_test_directory();
    let resource2 = Arc::new(MockCharOutput::new()) as Arc<dyn Resource>;

    let id1 = table.insert(resource1);
    let id2 = table.insert(resource2);

    // IDs should be different
    assert_ne!(id1, id2);

    // Should be able to get both handles
    assert!(table.get(id1).is_some());
    assert!(table.get(id2).is_some());

    // First should be Directory, second should be CharOutput
    assert!(table.get(id1).unwrap().as_directory().is_some());
    assert!(table.get(id2).unwrap().as_char_output().is_some());
}

fn handle_table_remove() {
    let mut table = HandleTable::new();

    let resource = create_test_directory();
    let id = table.insert(resource);

    // Should exist
    assert!(table.get(id).is_some());

    // Remove it
    let removed = table.remove(id);
    assert!(removed.is_some());

    // Should no longer exist
    assert!(table.get(id).is_none());

    // Double remove should return None
    assert!(table.remove(id).is_none());
}

fn handle_table_offset_state() {
    let mut table = HandleTable::new();

    let resource = create_test_directory();
    let id = table.insert(resource);

    // Initial offset should be 0
    assert_eq!(table.get(id).unwrap().offset(), 0);

    // Set offset
    table.get_mut(id).unwrap().set_offset(42);
    assert_eq!(table.get(id).unwrap().offset(), 42);

    // Offset persists across get calls
    table.get_mut(id).unwrap().set_offset(100);
    assert_eq!(table.get(id).unwrap().offset(), 100);
}

// Helper to create a mock process for buffer tests
fn create_test_process() -> panda_kernel::process::Process {
    use panda_kernel::process::context::Context;

    // Create a minimal ELF binary (just headers, no actual code)
    let mut elf_data = alloc::vec![0u8; 4096];

    // ELF magic number
    elf_data[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    // 64-bit
    elf_data[4] = 2;
    // Little endian
    elf_data[5] = 1;
    // Version
    elf_data[6] = 1;
    // e_type (ET_EXEC)
    elf_data[16] = 2;
    // e_machine (x86-64)
    elf_data[18] = 0x3e;
    // e_version
    elf_data[20] = 1;
    // e_entry (entry point at 0x400000)
    elf_data[24..32].copy_from_slice(&0x400000u64.to_le_bytes());
    // e_phoff (program header offset at 64)
    elf_data[32..40].copy_from_slice(&64u64.to_le_bytes());
    // e_ehsize (ELF header size)
    elf_data[52..54].copy_from_slice(&64u16.to_le_bytes());
    // e_phentsize (program header size)
    elf_data[54..56].copy_from_slice(&56u16.to_le_bytes());
    // e_phnum (0 program headers)
    elf_data[56..58].copy_from_slice(&0u16.to_le_bytes());

    let context = Context::new_user_context();
    let elf_slice: &[u8] = &elf_data;
    panda_kernel::process::Process::from_elf_data(context, elf_slice as *const [u8])
        .expect("Failed to create test process from ELF data")
}

fn buffer_free_list_no_merge() {
    let mut proc = create_test_process();

    // Allocate and free a range with no adjacent ranges
    let addr1 = proc.alloc_buffer_vaddr(2).unwrap(); // 2 pages at base
    proc.free_buffer_vaddr(addr1, 2);

    // Should be able to allocate from free list
    let addr2 = proc.alloc_buffer_vaddr(2).unwrap();
    assert_eq!(addr1, addr2, "Should reuse freed range");
}

fn buffer_free_list_merge_with_next() {
    let mut proc = create_test_process();

    // Allocate three ranges
    let addr1 = proc.alloc_buffer_vaddr(2).unwrap(); // 2 pages
    let addr2 = proc.alloc_buffer_vaddr(3).unwrap(); // 3 pages
    let addr3 = proc.alloc_buffer_vaddr(1).unwrap(); // 1 page

    // Free in order: addr2 then addr1 (should merge)
    proc.free_buffer_vaddr(addr2, 3);
    proc.free_buffer_vaddr(addr1, 2);

    // Should be able to allocate a 5-page range (merged)
    let addr4 = proc.alloc_buffer_vaddr(5).unwrap();
    assert_eq!(addr1, addr4, "Should allocate from merged range");

    // addr3 should still be allocated
    let _ = addr3;
}

fn buffer_free_list_merge_with_prev() {
    let mut proc = create_test_process();

    // Allocate three ranges
    let addr1 = proc.alloc_buffer_vaddr(2).unwrap(); // 2 pages
    let addr2 = proc.alloc_buffer_vaddr(3).unwrap(); // 3 pages
    let addr3 = proc.alloc_buffer_vaddr(1).unwrap(); // 1 page

    // Free in order: addr1 then addr2 (should merge)
    proc.free_buffer_vaddr(addr1, 2);
    proc.free_buffer_vaddr(addr2, 3);

    // Should be able to allocate a 5-page range (merged)
    let addr4 = proc.alloc_buffer_vaddr(5).unwrap();
    assert_eq!(addr1, addr4, "Should allocate from merged range");

    // addr3 should still be allocated
    let _ = addr3;
}

fn buffer_free_list_merge_both() {
    let mut proc = create_test_process();

    // Allocate four ranges
    let addr1 = proc.alloc_buffer_vaddr(2).unwrap(); // 2 pages
    let addr2 = proc.alloc_buffer_vaddr(3).unwrap(); // 3 pages
    let addr3 = proc.alloc_buffer_vaddr(1).unwrap(); // 1 page
    let addr4 = proc.alloc_buffer_vaddr(2).unwrap(); // 2 pages

    // Free addr1 and addr3 first
    proc.free_buffer_vaddr(addr1, 2);
    proc.free_buffer_vaddr(addr3, 1);

    // Now free addr2 (should merge with both)
    proc.free_buffer_vaddr(addr2, 3);

    // Should be able to allocate a 6-page range (2+3+1 merged)
    let addr5 = proc.alloc_buffer_vaddr(6).unwrap();
    assert_eq!(addr1, addr5, "Should allocate from fully merged range");

    // addr4 should still be allocated
    let _ = addr4;
}

fn buffer_free_list_partial_reuse() {
    let mut proc = create_test_process();

    // Allocate and free a 5-page range
    let addr1 = proc.alloc_buffer_vaddr(5).unwrap();
    proc.free_buffer_vaddr(addr1, 5);

    // Allocate 2 pages (should split the free range)
    let addr2 = proc.alloc_buffer_vaddr(2).unwrap();
    assert_eq!(addr1, addr2, "Should use start of free range");

    // Should still have 3 pages free
    let addr3 = proc.alloc_buffer_vaddr(3).unwrap();
    let expected_addr3 = x86_64::VirtAddr::new(addr1.as_u64() + (2 * 4096));
    assert_eq!(addr3, expected_addr3, "Should use remainder of split range");

    // Should not be able to allocate 4 pages from free list (only 3 available)
    let addr4 = proc.alloc_buffer_vaddr(1).unwrap();
    // This should come from bump allocation, not the free list
    let expected_addr4 = x86_64::VirtAddr::new(panda_abi::BUFFER_BASE as u64 + (5 * 4096));
    assert_eq!(
        addr4, expected_addr4,
        "Should use bump allocator when free list insufficient"
    );
}

fn buffer_free_list_multiple_free_ranges() {
    let mut proc = create_test_process();

    // Allocate 5 ranges
    let addr1 = proc.alloc_buffer_vaddr(1).unwrap();
    let addr2 = proc.alloc_buffer_vaddr(1).unwrap();
    let addr3 = proc.alloc_buffer_vaddr(1).unwrap();
    let addr4 = proc.alloc_buffer_vaddr(1).unwrap();
    let addr5 = proc.alloc_buffer_vaddr(1).unwrap();

    // Free alternating ranges: addr1, addr3, addr5
    proc.free_buffer_vaddr(addr1, 1);
    proc.free_buffer_vaddr(addr3, 1);
    proc.free_buffer_vaddr(addr5, 1);

    // Should have 3 separate 1-page free ranges
    // Allocate 1 page - should get first free range (addr1)
    let addr6 = proc.alloc_buffer_vaddr(1).unwrap();
    assert_eq!(addr6, addr1, "Should reuse first free range");

    // Allocate 1 page - should get second free range (addr3)
    let addr7 = proc.alloc_buffer_vaddr(1).unwrap();
    assert_eq!(addr7, addr3, "Should reuse second free range");

    // addr2 and addr4 should still be allocated
    let _ = (addr2, addr4);
}
