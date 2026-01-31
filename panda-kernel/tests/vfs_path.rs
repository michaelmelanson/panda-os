//! Integration tests for VFS path canonicalization.
//!
//! These tests mount a TarFS in-memory and verify that path traversal
//! via `..` components is properly prevented by the VFS layer.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use panda_kernel::vfs;
use panda_kernel::vfs::TarFs;

panda_kernel::test_harness!(
    canonical_path_unchanged,
    dot_components_stripped,
    dotdot_resolves_parent,
    dotdot_escape_changes_mount,
    dotdot_past_root_clamped,
    repeated_slashes_collapsed,
    open_with_dot_works,
    stat_canonicalizes,
    readdir_canonicalizes,
);

/// A no-op waker for busy-polling.
fn noop_waker() -> Waker {
    fn noop_clone(_: *const ()) -> RawWaker {
        RawWaker::new(core::ptr::null(), &NOOP_VTABLE)
    }
    fn noop(_: *const ()) {}

    static NOOP_VTABLE: RawWakerVTable = RawWakerVTable::new(noop_clone, noop, noop, noop);

    unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &NOOP_VTABLE)) }
}

/// Block on a future by polling once (TarFS always completes immediately).
fn block_on<T>(mut future: Pin<Box<dyn Future<Output = T>>>) -> T {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);

    match future.as_mut().poll(&mut cx) {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("TarFS future returned Pending"),
    }
}

// ---------------------------------------------------------------------------
// Build minimal ustar TAR archive in memory
// ---------------------------------------------------------------------------

/// Write bytes into a fixed-size buffer.
fn write_tar_str(buf: &mut [u8], s: &[u8]) {
    let len = s.len().min(buf.len());
    buf[..len].copy_from_slice(&s[..len]);
}

/// Compute a ustar header checksum.
fn tar_checksum(header: &[u8; 512]) -> [u8; 8] {
    let mut sum: u32 = 0;
    for (i, &b) in header.iter().enumerate() {
        if (148..156).contains(&i) {
            sum += b' ' as u32;
        } else {
            sum += b as u32;
        }
    }
    let mut result = [0u8; 8];
    let s = format!("{:06o}\0 ", sum);
    result[..8].copy_from_slice(&s.as_bytes()[..8]);
    result
}

/// Create a tar entry header for a regular file.
fn make_tar_header(name: &[u8], data_len: usize) -> [u8; 512] {
    let mut header = [0u8; 512];

    write_tar_str(&mut header[0..100], name);
    write_tar_str(&mut header[100..108], b"0000644\0");
    write_tar_str(&mut header[108..116], b"0000000\0");
    write_tar_str(&mut header[116..124], b"0000000\0");

    let size_str = format!("{:011o}\0", data_len);
    write_tar_str(&mut header[124..136], size_str.as_bytes());
    write_tar_str(&mut header[136..148], b"00000000000\0");

    header[156] = b'0'; // regular file

    write_tar_str(&mut header[257..263], b"ustar\0");
    write_tar_str(&mut header[263..265], b"00");

    let cksum = tar_checksum(&header);
    header[148..156].copy_from_slice(&cksum);

    header
}

// Static buffers for tar archives (must be 'static for TarFs pointers).
static mut TAR_BUF_1: [u8; 4096] = [0u8; 4096];
static mut TAR_BUF_2: [u8; 2048] = [0u8; 2048];
static MOUNTS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Ensure test mounts are set up exactly once.
///
/// Mount layout:
/// - `/test` -> TarFS with files: `hello.txt`, `sub/nested.txt`
/// - `/test/deep` -> TarFS with file: `secret.txt`
fn ensure_mounts() {
    if MOUNTS_INITIALIZED.swap(true, Ordering::SeqCst) {
        return; // Already initialized
    }

    // Build tar archive 1: hello.txt and sub/nested.txt
    let buf = unsafe { &mut *(&raw mut TAR_BUF_1) };
    let mut pos = 0;

    let content1 = b"Hello, world!";
    let header1 = make_tar_header(b"hello.txt", content1.len());
    buf[pos..pos + 512].copy_from_slice(&header1);
    pos += 512;
    buf[pos..pos + content1.len()].copy_from_slice(content1);
    pos += 512;

    let content2 = b"Nested content";
    let header2 = make_tar_header(b"sub/nested.txt", content2.len());
    buf[pos..pos + 512].copy_from_slice(&header2);
    pos += 512;
    buf[pos..pos + content2.len()].copy_from_slice(content2);

    let tar_data: *const [u8] = unsafe { &TAR_BUF_1[..] } as *const [u8];
    let fs = TarFs::from_tar_data(tar_data).expect("Failed to parse test TAR 1");
    vfs::mount("/test", Arc::new(fs));

    // Build tar archive 2: secret.txt
    let buf2 = unsafe { &mut *(&raw mut TAR_BUF_2) };
    let content3 = b"Secret file";
    let header3 = make_tar_header(b"secret.txt", content3.len());
    buf2[0..512].copy_from_slice(&header3);
    buf2[512..512 + content3.len()].copy_from_slice(content3);

    let tar_data2: *const [u8] = unsafe { &TAR_BUF_2[..] } as *const [u8];
    let fs2 = TarFs::from_tar_data(tar_data2).expect("Failed to parse test TAR 2");
    vfs::mount("/test/deep", Arc::new(fs2));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

fn canonical_path_unchanged() {
    ensure_mounts();
    let result = block_on(Box::pin(vfs::open("/test/hello.txt")));
    assert!(result.is_ok(), "Normal path /test/hello.txt should open");
}

fn dot_components_stripped() {
    ensure_mounts();
    let result = block_on(Box::pin(vfs::open("/test/./hello.txt")));
    assert!(result.is_ok(), "/test/./hello.txt should open");
}

fn dotdot_resolves_parent() {
    ensure_mounts();
    // /test/sub/../hello.txt -> /test/hello.txt
    let result = block_on(Box::pin(vfs::open("/test/sub/../hello.txt")));
    assert!(result.is_ok(), "/test/sub/../hello.txt should open");
}

fn dotdot_escape_changes_mount() {
    ensure_mounts();
    // /test/deep/../hello.txt canonicalizes to /test/hello.txt
    // Should match /test mount, NOT pass "../hello.txt" to /test/deep's TarFS
    let result = block_on(Box::pin(vfs::open("/test/deep/../hello.txt")));
    assert!(
        result.is_ok(),
        "/test/deep/../hello.txt should resolve to /test/hello.txt"
    );
}

fn dotdot_past_root_clamped() {
    ensure_mounts();
    // /../../../test/hello.txt -> /test/hello.txt
    let result = block_on(Box::pin(vfs::open("/../../../test/hello.txt")));
    assert!(
        result.is_ok(),
        "/../../../test/hello.txt should resolve to /test/hello.txt"
    );
}

fn repeated_slashes_collapsed() {
    ensure_mounts();
    let result = block_on(Box::pin(vfs::open("///test//hello.txt")));
    assert!(result.is_ok(), "///test//hello.txt should open");
}

fn open_with_dot_works() {
    ensure_mounts();
    let result = block_on(Box::pin(vfs::open("/test/deep/./secret.txt")));
    assert!(result.is_ok(), "/test/deep/./secret.txt should open");
}

fn stat_canonicalizes() {
    ensure_mounts();
    let result = block_on(Box::pin(vfs::stat("/test/./hello.txt")));
    assert!(result.is_ok(), "stat on /test/./hello.txt should work");
    let stat = result.unwrap();
    assert!(!stat.is_dir);
    assert!(stat.size > 0);
}

fn readdir_canonicalizes() {
    ensure_mounts();
    // /test/sub/.. -> /test
    let result = block_on(Box::pin(vfs::readdir("/test/sub/..")));
    assert!(
        result.is_ok(),
        "readdir on /test/sub/.. should resolve to /test"
    );
    let entries = result.unwrap();
    assert!(!entries.is_empty(), "/test should have entries");
}
