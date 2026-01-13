//! Tests for the Resource trait and interface traits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;

use panda_kernel::resource::{Block, BlockError, DirEntry, DirectoryResource, Resource};

panda_kernel::test_harness!(
    directory_interface_available,
    directory_entry_access,
    directory_count,
    directory_entry_past_end,
    mock_block_interface,
    mock_block_read,
    mock_block_size,
    resource_interface_dispatch,
);

// =============================================================================
// Mock Block resource for testing
// =============================================================================

/// A mock block resource with fixed content for testing.
struct MockBlock {
    data: &'static [u8],
}

impl MockBlock {
    fn new(data: &'static [u8]) -> Self {
        Self { data }
    }
}

impl Resource for MockBlock {
    fn as_block(&self) -> Option<&dyn Block> {
        Some(self)
    }
}

impl Block for MockBlock {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, BlockError> {
        let offset = offset as usize;
        if offset >= self.data.len() {
            return Ok(0);
        }
        let remaining = self.data.len() - offset;
        let to_read = buf.len().min(remaining);
        buf[..to_read].copy_from_slice(&self.data[offset..offset + to_read]);
        Ok(to_read)
    }

    fn size(&self) -> u64 {
        self.data.len() as u64
    }
}

// =============================================================================
// Test fixtures
// =============================================================================

/// Create a test block resource
fn create_test_block() -> Box<dyn Resource> {
    Box::new(MockBlock::new(b"Hello from test data!"))
}

/// Create a test directory resource
fn create_test_directory() -> Box<dyn Resource> {
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
    Box::new(DirectoryResource::new(entries))
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
        resource.as_block().is_none(),
        "Directory resource should not implement Block"
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
// Block interface tests
// =============================================================================

fn mock_block_interface() {
    let resource = create_test_block();
    assert!(
        resource.as_block().is_some(),
        "Block resource should implement Block"
    );
    assert!(
        resource.as_event_source().is_none(),
        "Block resource should not implement EventSource"
    );
    assert!(
        resource.as_directory().is_none(),
        "Block resource should not implement Directory"
    );
}

fn mock_block_read() {
    let resource = create_test_block();
    let block = resource.as_block().unwrap();

    // Read from start
    let mut buf = [0u8; 5];
    let n = block.read_at(0, &mut buf).expect("Read should succeed");
    assert_eq!(n, 5);
    assert_eq!(&buf, b"Hello");

    // Read from offset
    let mut buf = [0u8; 4];
    let n = block.read_at(6, &mut buf).expect("Read should succeed");
    assert_eq!(n, 4);
    assert_eq!(&buf, b"from");

    // Read past EOF
    let size = block.size();
    let mut buf = [0u8; 10];
    let n = block
        .read_at(size, &mut buf)
        .expect("Read at EOF should succeed");
    assert_eq!(n, 0);
}

fn mock_block_size() {
    let resource = create_test_block();
    let block = resource.as_block().unwrap();

    // "Hello from test data!" = 21 bytes
    assert_eq!(block.size(), 21);
}

// =============================================================================
// Resource dispatch tests
// =============================================================================

fn resource_interface_dispatch() {
    let block = create_test_block();
    let dir = create_test_directory();

    // Block: has Block, not Directory
    assert!(block.as_block().is_some());
    assert!(block.as_directory().is_none());

    // Directory: has Directory, not Block
    assert!(dir.as_directory().is_some());
    assert!(dir.as_block().is_none());

    // Both can be treated as dyn Resource
    fn check_resource(_r: &dyn Resource) {}
    check_resource(block.as_ref());
    check_resource(dir.as_ref());
}
