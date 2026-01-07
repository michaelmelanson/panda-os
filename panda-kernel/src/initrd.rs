//! Initrd (initial ramdisk) filesystem support.
//!
//! Provides access to binaries bundled in the boot TAR archive.

use alloc::collections::BTreeMap;
use alloc::string::String;
use spinning_top::RwSpinlock;
use tar_no_std::TarArchiveRef;

/// Global initrd instance, initialized at boot
static INITRD: RwSpinlock<Option<Initrd>> = RwSpinlock::new(None);

/// Parsed initrd containing file name to data mappings
pub struct Initrd {
    /// Maps filename (without leading path) to (offset, length) in original data
    files: BTreeMap<String, (*const u8, usize)>,
    /// Raw archive data pointer (kept alive by UEFI allocation)
    _data: *const [u8],
}

// Safety: The data pointer comes from UEFI allocation that persists for kernel lifetime
unsafe impl Send for Initrd {}
unsafe impl Sync for Initrd {}

impl Initrd {
    /// Parse a TAR archive and build the file index
    pub fn from_tar_data(data: *const [u8]) -> Self {
        let bytes = unsafe { data.as_ref().unwrap() };
        let archive = TarArchiveRef::new(bytes).expect("Failed to parse initrd TAR archive");

        let mut files = BTreeMap::new();

        for entry in archive.entries() {
            let filename = entry.filename();
            let name = filename.as_str().expect("Invalid UTF-8 in filename");
            if name.is_empty() || name.ends_with('/') {
                continue; // Skip directory entries
            }

            let data_ptr = entry.data().as_ptr();
            let data_len = entry.data().len();
            files.insert(String::from(name), (data_ptr, data_len));
        }

        Initrd { files, _data: data }
    }

    /// Look up a binary by name, returning its data as a slice pointer
    pub fn get(&self, name: &str) -> Option<*const [u8]> {
        self.files
            .get(name)
            .map(|(ptr, len)| core::ptr::slice_from_raw_parts(*ptr, *len))
    }
}

/// Initialize the global initrd from TAR archive data.
/// Must be called after memory initialization.
pub fn init(data: *const [u8]) {
    let initrd = Initrd::from_tar_data(data);
    let mut global = INITRD.write();
    assert!(global.is_none(), "initrd already initialized");
    *global = Some(initrd);
}

/// Get binary data by name from the initrd
pub fn get(name: &str) -> Option<*const [u8]> {
    INITRD.read().as_ref().and_then(|i| i.get(name))
}

/// Get the init binary (convenience function)
pub fn get_init() -> *const [u8] {
    get("init").expect("initrd must contain 'init'")
}
