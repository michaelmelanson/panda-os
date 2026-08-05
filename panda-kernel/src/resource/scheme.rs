//! Resource scheme system for unified resource access.
//!
//! Resources are identified by URIs with a scheme and path:
//! - `file:/initrd/init` -> File via existing VFS/mount system
//! - `console:/serial/0` -> Serial console device
//!
//! The scheme identifies the resource type, and the path is the address
//! within that scheme's namespace.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use async_trait::async_trait;
use spinning_top::{RwSpinlock, Spinlock};
use x86_64::instructions::port::Port;

use crate::device_path;
use crate::devices::claims::{ClaimGuard, ClaimOwner};
use crate::devices::virtio_block;
use crate::devices::virtio_keyboard::{self, VirtioKeyboard};
use crate::process::waker::IoWaker;
use crate::vfs;

use super::Resource;
use super::char_output::{CharOutError, CharacterOutput};
use super::directory::{DirEntry, Directory};
use super::event_source::{Event, EventSource, KeyEvent};

/// Error returned when opening a resource via a scheme fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// No resource exists at the given URI.
    NotFound,
    /// The resource exists but is exclusively claimed by another owner
    /// (see `crate::devices::claims`).
    Busy,
}

/// A handler for a resource scheme (e.g., "file", "console", "pci")
#[async_trait]
pub trait SchemeHandler: Send + Sync {
    /// Open a resource at the given path within this scheme
    async fn open(&self, path: &str) -> Result<Box<dyn Resource>, OpenError>;

    /// List directory contents at the given path within this scheme
    async fn readdir(&self, _path: &str) -> Option<Vec<DirEntry>> {
        None
    }

    /// Connect to this scheme and receive a live channel to it, per
    /// `panda_abi::scheme_protocol::Request::Connect`'s doc comment.
    ///
    /// Unlike [`open`](Self::open), which returns a resource this process
    /// owns exclusively, `connect` returns a shared `Arc` because the
    /// concrete resource (for [`UserSchemeProvider`], a fresh
    /// `ChannelEndpoint` the provider handed over as a channel attachment)
    /// is installed directly into the *caller's* handle table by
    /// `syscall::environment::handle_connect` — there is no intermediate
    /// proxy for this path to own. Schemes with no connect-style resource
    /// (everything except [`UserSchemeProvider`] today) inherit the default,
    /// which always fails `NotFound`.
    async fn connect(&self, _path: &str) -> Result<Arc<dyn Resource>, OpenError> {
        Err(OpenError::NotFound)
    }
}

/// Global registry of scheme handlers.
///
/// Keyed by owned `String` rather than `&'static str`: M2.2 (userspace
/// providers) needs to register scheme names discovered at runtime, which
/// don't have `'static` lifetimes. Callers registering compile-time scheme
/// names (string literals) are unaffected — `register_scheme` accepts
/// anything convertible to `String`.
static SCHEMES: RwSpinlock<BTreeMap<String, Arc<dyn SchemeHandler>>> =
    RwSpinlock::new(BTreeMap::new());

/// Register a scheme handler.
pub fn register_scheme(name: impl Into<String>, handler: Arc<dyn SchemeHandler>) {
    let mut schemes = SCHEMES.write();
    schemes.insert(name.into(), handler);
}

/// List the names of all currently registered schemes, sorted.
///
/// `BTreeMap` already iterates in key order, so this is sorted "for free" —
/// no separate sort step needed.
pub fn scheme_names() -> Vec<String> {
    SCHEMES.read().keys().cloned().collect()
}

/// Register a userspace-provided scheme backed by `kernel_endpoint` (see
/// `syscall::scheme::handle_register` / `OP_SCHEME_REGISTER`).
///
/// Rejects an empty name or a name that is already registered — checked and
/// inserted under a single write-lock scope so there's no
/// check-then-insert race with a concurrent registration of the same name.
pub fn register_user_scheme(
    name: String,
    kernel_endpoint: super::ChannelEndpoint,
) -> Result<(), panda_abi::ErrorCode> {
    if name.is_empty() {
        return Err(panda_abi::ErrorCode::InvalidArgument);
    }
    let mut schemes = SCHEMES.write();
    if schemes.contains_key(&name) {
        return Err(panda_abi::ErrorCode::AlreadyExists);
    }
    schemes.insert(name, Arc::new(UserSchemeProvider::new(kernel_endpoint)));
    Ok(())
}

/// Remove a scheme registered via [`register_user_scheme`], if present.
///
/// This is strictly an internal rollback helper for `syscall::scheme::handle_register`'s
/// `TooManyHandles` failure path (registration succeeds, then the provider's
/// handle-table insert fails, leaving the scheme registered but with no
/// process able to reach the dropped provider endpoint). It is deliberately
/// **not** a general-purpose scheme-unregistration feature — that was
/// explicitly deferred out of M2.2's scope — hence `pub(crate)` rather than
/// a syscall or public API.
pub(crate) fn unregister_scheme_if_present(name: &str) {
    SCHEMES.write().remove(name);
}

/// Open a resource by URI (e.g., "file:/initrd/init" or "console:/serial/0")
pub async fn open(uri: &str) -> Result<Box<dyn Resource>, OpenError> {
    let (scheme, path) = uri.split_once(':').ok_or(OpenError::NotFound)?;
    // Clone the handler to avoid holding the lock across await
    let handler: Arc<dyn SchemeHandler> = {
        let schemes = SCHEMES.read();
        schemes
            .get(scheme)
            .map(|h| Arc::clone(h))
            .ok_or(OpenError::NotFound)?
    };
    handler.open(path).await
}

/// Connect to a scheme by URI (e.g., "compositor:/connect") and receive a
/// live channel to its provider. See `SchemeHandler::connect`.
pub async fn connect(uri: &str) -> Result<Arc<dyn Resource>, OpenError> {
    let (scheme, path) = uri.split_once(':').ok_or(OpenError::NotFound)?;
    let handler: Arc<dyn SchemeHandler> = {
        let schemes = SCHEMES.read();
        schemes
            .get(scheme)
            .map(|h| Arc::clone(h))
            .ok_or(OpenError::NotFound)?
    };
    handler.connect(path).await
}

/// List directory contents by URI (e.g., "file:/initrd")
pub async fn readdir(uri: &str) -> Option<Vec<DirEntry>> {
    let (scheme, path) = uri.split_once(':')?;

    // Clone the handler to avoid holding the lock across await
    let handler: Arc<dyn SchemeHandler> = {
        let schemes = SCHEMES.read();
        schemes.get(scheme).map(|h| Arc::clone(h))?
    };
    handler.readdir(path).await
}

// =============================================================================
// File Scheme - wraps existing VFS
// =============================================================================

/// Scheme handler that wraps the existing VFS mount system
pub struct FileScheme;

#[async_trait]
impl SchemeHandler for FileScheme {
    async fn open(&self, path: &str) -> Result<Box<dyn Resource>, OpenError> {
        // Try it as a directory first. This resolves the path with a single
        // lookup: both ext2 and TarFs fail `readdir` with `NotFound` for a
        // path that names a file (or doesn't exist), so a non-directory
        // falls straight through to the file-open path below without a
        // separate `stat` call to decide which lookup to do.
        if let Ok(entries) = vfs::readdir(path).await {
            // Return a directory resource with VFS path for mutation support
            return Ok(Box::new(DirectoryResource::with_vfs_path(
                entries,
                alloc::string::String::from(path),
            )));
        }

        // Open as a file
        let file = vfs::open(path).await.map_err(|_| OpenError::NotFound)?;
        Ok(Box::new(VfsFileResource::new(file)))
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        vfs::readdir(path).await.ok()
    }
}

/// A file resource wrapping a VFS file.
pub struct VfsFileResource {
    file: Spinlock<Box<dyn vfs::File>>,
}

impl VfsFileResource {
    pub fn new(file: Box<dyn vfs::File>) -> Self {
        Self {
            file: Spinlock::new(file),
        }
    }
}

impl Resource for VfsFileResource {
    fn handle_type(&self) -> panda_abi::HandleType {
        panda_abi::HandleType::File
    }

    fn as_vfs_file(&self) -> Option<&dyn super::VfsFile> {
        Some(self)
    }
}

impl super::VfsFile for VfsFileResource {
    fn file(&self) -> &Spinlock<Box<dyn vfs::File>> {
        &self.file
    }
}

/// A directory resource.
///
/// If `vfs_path` is set, this directory supports mutation operations
/// (create, unlink) via the VFS layer.
pub struct DirectoryResource {
    entries: Vec<DirEntry>,
    /// Absolute VFS path of this directory, if backed by a VFS filesystem.
    vfs_path: Option<alloc::string::String>,
}

impl DirectoryResource {
    pub fn new(entries: Vec<DirEntry>) -> Self {
        Self {
            entries,
            vfs_path: None,
        }
    }

    /// Create a directory resource with an associated VFS path for mutation.
    pub fn with_vfs_path(entries: Vec<DirEntry>, path: alloc::string::String) -> Self {
        Self {
            entries,
            vfs_path: Some(path),
        }
    }
}

impl Resource for DirectoryResource {
    fn handle_type(&self) -> panda_abi::HandleType {
        panda_abi::HandleType::Directory
    }

    fn as_directory(&self) -> Option<&dyn Directory> {
        Some(self)
    }
}

impl Directory for DirectoryResource {
    fn entry(&self, index: usize) -> Option<DirEntry> {
        self.entries.get(index).cloned()
    }

    fn count(&self) -> usize {
        self.entries.len()
    }

    fn vfs_path(&self) -> Option<alloc::string::String> {
        self.vfs_path.clone()
    }
}

// =============================================================================
// Console Scheme - serial console access
// =============================================================================

/// Scheme handler for console devices
pub struct ConsoleScheme;

#[async_trait]
impl SchemeHandler for ConsoleScheme {
    async fn open(&self, path: &str) -> Result<Box<dyn Resource>, OpenError> {
        match path {
            "/serial/0" => Ok(Box::new(SerialConsoleResource::new(0x3f8))),
            _ => Err(OpenError::NotFound),
        }
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        match path {
            "/" => Some(alloc::vec![DirEntry {
                name: alloc::string::String::from("serial"),
                is_dir: true,
            }]),
            "/serial" => Some(alloc::vec![DirEntry {
                name: alloc::string::String::from("0"),
                is_dir: false,
            }]),
            _ => None,
        }
    }
}

/// A serial console resource
pub struct SerialConsoleResource {
    port: u16,
}

impl SerialConsoleResource {
    pub fn new(port: u16) -> Self {
        Self { port }
    }
}

impl Resource for SerialConsoleResource {
    fn handle_type(&self) -> panda_abi::HandleType {
        // Console is accessed like a file
        panda_abi::HandleType::File
    }

    fn as_char_output(&self) -> Option<&dyn CharacterOutput> {
        Some(self)
    }
}

impl CharacterOutput for SerialConsoleResource {
    fn write(&self, buf: &[u8]) -> Result<usize, CharOutError> {
        for &byte in buf {
            unsafe {
                Port::new(self.port).write(byte);
            }
        }
        Ok(buf.len())
    }
}

// =============================================================================
// Keyboard Scheme - virtio keyboard access
// =============================================================================

/// Scheme handler for keyboard devices
pub struct KeyboardScheme;

#[async_trait]
impl SchemeHandler for KeyboardScheme {
    async fn open(&self, path: &str) -> Result<Box<dyn Resource>, OpenError> {
        // Resolve path like "/pci/input/0" or "/pci/00:03.0"
        let address = device_path::resolve(path).ok_or(OpenError::NotFound)?;
        let keyboard = virtio_keyboard::get_keyboard(&address).ok_or(OpenError::NotFound)?;
        Ok(Box::new(KeyboardResource { keyboard }))
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        device_path::list(path)
    }
}

/// Handle to an open keyboard device
pub struct KeyboardResource {
    keyboard: Arc<Spinlock<VirtioKeyboard>>,
}

impl Resource for KeyboardResource {
    fn handle_type(&self) -> panda_abi::HandleType {
        // Keyboard is accessed like a file (read events)
        panda_abi::HandleType::File
    }

    fn as_event_source(&self) -> Option<&dyn EventSource> {
        Some(self)
    }

    fn waker(&self) -> Option<Arc<IoWaker>> {
        Some(self.keyboard.lock().waker())
    }

    fn supported_events(&self) -> u32 {
        panda_abi::EVENT_KEYBOARD_KEY
    }

    fn poll_events(&self) -> u32 {
        // The ring buffer only exposes a has-pending check, not per-event
        // peeking, so we report the coarse "at least one key event is
        // buffered" state rather than inventing additional buffering here.
        // Mailbox delivery of individual key events still happens eagerly
        // via the wake/post mechanism in `VirtioKeyboard::poll`, independent
        // of this method.
        if self.keyboard.lock().has_events() {
            panda_abi::EVENT_KEYBOARD_KEY
        } else {
            0
        }
    }

    fn attach_mailbox(&self, mailbox_ref: super::MailboxRef) {
        self.keyboard.lock().attach_mailbox(mailbox_ref);
    }
}

impl EventSource for KeyboardResource {
    fn poll(&self) -> Option<Event> {
        let mut kb = self.keyboard.lock();
        kb.pop_event().map(|event| {
            Event::Key(KeyEvent {
                code: event.code,
                value: event.value,
            })
        })
    }

    fn waker(&self) -> Arc<IoWaker> {
        self.keyboard.lock().waker()
    }
}

// =============================================================================
// Display Scheme - exclusive display ownership
// =============================================================================

/// Scheme handler for display devices (`display:/pci/display/0`).
///
/// Opening a display claims it exclusively (see
/// `crate::resource::display`): a second open fails with `Busy` until the
/// owning handle is closed or the owning process exits.
pub struct DisplayScheme;

#[async_trait]
impl SchemeHandler for DisplayScheme {
    async fn open(&self, path: &str) -> Result<Box<dyn Resource>, OpenError> {
        // Resolve like any other class-based device path, e.g.
        // "/pci/display/0" or the raw "/pci/00:02.0".
        let address = device_path::resolve(path).ok_or(OpenError::NotFound)?;

        // Only the device the framebuffer was initialized from can be opened
        // as a display; any other PCI device resolved by this path is not a
        // display this kernel can drive.
        if super::display_device_address().as_ref() != Some(&address) {
            return Err(OpenError::NotFound);
        }

        let claim = crate::devices::claims::claim(address, ClaimOwner::Display)
            .map_err(|_| OpenError::Busy)?;

        let display = super::DisplayDevice::new(claim).ok_or(OpenError::NotFound)?;
        Ok(Box::new(display))
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        device_path::list(path)
    }
}

// =============================================================================
// Block Scheme - block device access
// =============================================================================

/// Scheme handler for block devices (virtio-blk, future AHCI, NVMe).
///
/// Paths support both raw addresses and class-based resolution:
/// - `/pci/00:04.0` - raw PCI address
/// - `/pci/storage/0` - first storage device
pub struct BlockScheme;

#[async_trait]
impl SchemeHandler for BlockScheme {
    async fn open(&self, path: &str) -> Result<Box<dyn Resource>, OpenError> {
        // Resolve path like "/pci/storage/0" or "/pci/00:04.0"
        let address = device_path::resolve(path).ok_or(OpenError::NotFound)?;

        // Claim the device before handing out raw access to it. If a
        // filesystem is mounted on this device (see `vfs::mount_ext2`,
        // which holds a `Mount`-tagged claim for as long as the mount is
        // active), this fails with `Busy` instead of minting a second,
        // unsynchronized writer underneath the mounted filesystem.
        let claim = crate::devices::claims::claim(address.clone(), ClaimOwner::RawOpen)
            .map_err(|_| OpenError::Busy)?;

        // Try virtio-blk registry (future: try AHCI, NVMe registries too)
        let device = virtio_block::get_device(&address).ok_or(OpenError::NotFound)?;
        let device: Arc<dyn super::BlockDevice> = Arc::new(device);

        // Wrap in a VFS file for async access
        let file: Box<dyn vfs::File> = Box::new(vfs::BlockDeviceFile::new(device));
        Ok(Box::new(BlockDeviceResource {
            file: Spinlock::new(file),
            _claim: claim,
        }))
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        device_path::list(path)
    }
}

/// Resource wrapper for a block device.
///
/// Block devices are exposed through the VFS file interface for async I/O.
/// Holds the device's exclusive claim for the lifetime of this resource;
/// dropping it (on `close()` or process exit) releases the claim.
struct BlockDeviceResource {
    file: Spinlock<Box<dyn vfs::File>>,
    _claim: ClaimGuard,
}

impl Resource for BlockDeviceResource {
    fn handle_type(&self) -> panda_abi::HandleType {
        panda_abi::HandleType::File
    }

    fn as_vfs_file(&self) -> Option<&dyn super::VfsFile> {
        Some(self)
    }
}

impl super::VfsFile for BlockDeviceResource {
    fn file(&self) -> &Spinlock<Box<dyn vfs::File>> {
        &self.file
    }
}

// =============================================================================
// Scheme Scheme - meta-scheme registry enumeration
// =============================================================================

/// Scheme handler for the `scheme:` meta-scheme.
///
/// This is the honest replacement for the removed `*:` discovery hack:
/// `readdir("scheme:/")` lists the names of every registered scheme handler
/// (including "scheme" itself, since it's registered the same way as
/// everything else via `register_scheme`).
///
/// Nothing is open-able here yet: `scheme:/<name>` is reserved namespace for
/// M2.2's userspace-provider metadata (e.g. which process backs a scheme,
/// its capabilities). Until that lands, every `open` — including the root —
/// fails `NotFound`.
pub struct SchemeScheme;

#[async_trait]
impl SchemeHandler for SchemeScheme {
    async fn open(&self, _path: &str) -> Result<Box<dyn Resource>, OpenError> {
        Err(OpenError::NotFound)
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        match path {
            "/" => Some(
                scheme_names()
                    .into_iter()
                    .map(|name| DirEntry { name, is_dir: false })
                    .collect(),
            ),
            _ => None,
        }
    }
}

// =============================================================================
// User scheme provider — routes scheme requests to a userspace process
// over a channel (M2.2).
// =============================================================================

/// A minimal async mutual-exclusion lock built on the same "poll, if busy
/// register waiter and return Pending" idiom the rest of the kernel uses for
/// process blocking (see `syscall/channel.rs`'s handlers).
///
/// This backs the v1 "one request in flight per provider" concurrency
/// scope described on [`ProviderState::round_trip`]: it is deliberately not
/// a fair queue, just a spin-and-retry lock, which is fine given the
/// kernel's single-core cooperative scheduling.
///
/// Unlike [`IoWaker`] (which tracks a single waiting process — fine for its
/// original one-device/one-waiter use cases), this lock can have multiple
/// concurrent waiters: several client processes can all be blocked trying to
/// reach the same userspace scheme provider at once. So waiters are tracked
/// in a `Vec` rather than a single slot, and release wakes *all* of them,
/// letting them re-race the `compare_exchange` when re-polled. This is a
/// "thundering herd" on release, but with this kernel's single-core
/// cooperative scheduling the herd is bounded (by the number of genuine
/// waiters) and harmless — and simpler than a real FIFO queue, which this
/// lock doesn't need or attempt to be.
struct AsyncLock {
    busy: core::sync::atomic::AtomicBool,
    waiters: Spinlock<Vec<crate::process::ProcessId>>,
}

impl AsyncLock {
    fn new() -> Self {
        Self {
            busy: core::sync::atomic::AtomicBool::new(false),
            waiters: Spinlock::new(Vec::new()),
        }
    }

    async fn acquire(&self) -> AsyncLockGuard<'_> {
        core::future::poll_fn(|_cx| {
            use core::sync::atomic::Ordering;
            if self
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                core::task::Poll::Ready(())
            } else {
                let pid = crate::scheduler::current_process_id();
                let mut waiters = self.waiters.lock();
                // Dedup: the same process re-polling without a fresh wakeup
                // shouldn't accumulate duplicate entries in the wake-all set.
                if !waiters.contains(&pid) {
                    waiters.push(pid);
                }
                core::task::Poll::Pending
            }
        })
        .await;
        AsyncLockGuard { lock: self }
    }
}

struct AsyncLockGuard<'a> {
    lock: &'a AsyncLock,
}

impl Drop for AsyncLockGuard<'_> {
    fn drop(&mut self) {
        self.lock.busy.store(false, core::sync::atomic::Ordering::Release);
        let waiters: Vec<_> = core::mem::take(&mut *self.lock.waiters.lock());
        for pid in waiters {
            crate::scheduler::wake_process(pid);
        }
    }
}

/// Errors from a provider round trip, distinct from the wire-level
/// `panda_abi::ErrorCode` the provider itself reports for a given request:
/// these describe the round trip failing to happen at all.
#[derive(Debug, Clone, Copy)]
enum ProviderError {
    /// The provider process has exited (or otherwise closed its endpoint of
    /// the channel) — see docs/SYSCALLS.md "Scheme provider operations":
    /// pending and future requests must fail cleanly rather than hang.
    Disconnected,
    /// The provider sent something that didn't decode as a valid response.
    Protocol,
}

fn provider_error_to_error_code(err: ProviderError) -> panda_abi::ErrorCode {
    match err {
        ProviderError::Disconnected => panda_abi::ErrorCode::IoError,
        ProviderError::Protocol => panda_abi::ErrorCode::Protocol,
    }
}

/// Shared state for one registered userspace scheme provider.
struct ProviderState {
    /// The kernel's endpoint of the channel; the provider process holds the
    /// other endpoint and serves requests with the ordinary
    /// `OP_CHANNEL_SEND`/`OP_CHANNEL_RECV` syscalls it already has.
    kernel_endpoint: super::ChannelEndpoint,
    /// Serializes request/response round trips to this provider (v1 scope:
    /// single request in flight at a time — see `round_trip`).
    lock: AsyncLock,
    next_request_id: core::sync::atomic::AtomicU64,
}

impl ProviderState {
    fn new(kernel_endpoint: super::ChannelEndpoint) -> Self {
        Self {
            kernel_endpoint,
            lock: AsyncLock::new(),
            next_request_id: core::sync::atomic::AtomicU64::new(1),
        }
    }

    fn next_request_id(&self) -> u64 {
        self.next_request_id
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
    }

    /// Send `request` and wait for the matching response.
    ///
    /// **v1 concurrency scope**: this holds `self.lock` for the whole
    /// send-then-receive round trip, so two callers hitting the same
    /// provider concurrently queue behind each other here rather than being
    /// multiplexed by `request_id`. Future work: a `request_id`-keyed
    /// pending-response map plus a router task would let independent
    /// requests to the same provider proceed concurrently; not implemented
    /// here, deliberately, to keep this correct and easy to reason about.
    ///
    /// `request_id` is still threaded through the wire format even though
    /// v1 doesn't need it for correlation *within a single round trip* — it
    /// pays for itself in exactly the situation handled below: a
    /// fire-and-forget `Close` (see `SchemeProxyResource`'s `Drop`) can
    /// leave an orphaned response in the queue ahead of a later request's
    /// real response. Any response whose `request_id` doesn't match this
    /// call's own is silently discarded and the receive retried, rather
    /// than being misdelivered to the wrong caller.
    ///
    /// Delegates to [`round_trip_with_attachment`](Self::round_trip_with_attachment)
    /// and discards the attachment — every request kind except `Connect`
    /// ignores it.
    async fn round_trip(&self, request_id: u64, request: &[u8]) -> Result<Vec<u8>, ProviderError> {
        self.round_trip_with_attachment(request_id, request)
            .await
            .map(|(buf, _attachment)| buf)
    }

    /// As [`round_trip`](Self::round_trip), but also reports any resource
    /// the provider attached to its response — see
    /// `Response::ConnectOk`/`UserSchemeProvider::connect`, the only caller
    /// that currently uses the attachment.
    async fn round_trip_with_attachment(
        &self,
        request_id: u64,
        request: &[u8],
    ) -> Result<(Vec<u8>, Option<Arc<dyn Resource>>), ProviderError> {
        let _guard = self.lock.acquire().await;

        self.kernel_endpoint
            .send(request)
            .map_err(|_| ProviderError::Disconnected)?;

        loop {
            let mut buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
            let (len, attachment) = core::future::poll_fn(|_cx| {
                let mut registered = false;
                loop {
                    match self.kernel_endpoint.recv_with_attachment(&mut buf) {
                        Ok((len, attachment)) => {
                            return core::task::Poll::Ready(Ok((len, attachment)));
                        }
                        Err(super::ChannelError::QueueEmpty) => {
                            if registered {
                                return core::task::Poll::Pending;
                            }
                            // Register, then retry immediately: the
                            // provider's response may arrive (and call
                            // `wake()`) between the `recv` above and this
                            // registration, in which case `wake()` finds no
                            // registered waiter and the response would
                            // otherwise be missed, leaving this round trip
                            // blocked forever.
                            self.kernel_endpoint
                                .waker()
                                .set_waiting(crate::scheduler::current_process_id());
                            registered = true;
                        }
                        Err(super::ChannelError::PeerClosed) => {
                            return core::task::Poll::Ready(Err(ProviderError::Disconnected));
                        }
                        Err(_) => return core::task::Poll::Ready(Err(ProviderError::Protocol)),
                    }
                }
            })
            .await?;
            buf.truncate(len);

            if buf.len() < 9 {
                return Err(ProviderError::Protocol);
            }
            let response_id = u64::from_le_bytes(buf[1..9].try_into().unwrap());
            if response_id == request_id {
                return Ok((buf, attachment));
            }
            // Orphaned response (see doc comment above) — keep waiting for
            // the one that actually matches this call.
        }
    }
}

/// `SchemeHandler` that routes `open`/`readdir` to a userspace provider
/// process over a channel, per `panda_abi::scheme_protocol`.
pub struct UserSchemeProvider {
    state: Arc<ProviderState>,
}

impl UserSchemeProvider {
    fn new(kernel_endpoint: super::ChannelEndpoint) -> Self {
        Self {
            state: Arc::new(ProviderState::new(kernel_endpoint)),
        }
    }
}

#[async_trait]
impl SchemeHandler for UserSchemeProvider {
    async fn open(&self, path: &str) -> Result<Box<dyn Resource>, OpenError> {
        use panda_abi::scheme_protocol::{Request, Response};

        let request_id = self.state.next_request_id();
        let mut req_buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
        let Some(n) = (Request::Open { request_id, path }).encode(&mut req_buf) else {
            return Err(OpenError::NotFound);
        };
        req_buf.truncate(n);

        let resp = self
            .state
            .round_trip(request_id, &req_buf)
            .await
            .map_err(|_| OpenError::NotFound)?;

        match Response::decode(&resp) {
            Some(Response::OpenOk { resource_id, .. }) => Ok(Box::new(SchemeProxyResource {
                provider: Arc::clone(&self.state),
                resource_id,
            })),
            Some(Response::OpenErr { error, .. }) if error == panda_abi::ErrorCode::Busy => {
                Err(OpenError::Busy)
            }
            _ => Err(OpenError::NotFound),
        }
    }

    async fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        use panda_abi::scheme_protocol::{Request, Response};

        let request_id = self.state.next_request_id();
        let mut req_buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
        let n = (Request::Readdir { request_id, path }).encode(&mut req_buf)?;
        req_buf.truncate(n);

        let resp = self.state.round_trip(request_id, &req_buf).await.ok()?;

        match Response::decode(&resp) {
            Some(Response::ReaddirOk { raw, .. }) => {
                let entries = panda_abi::scheme_protocol::ReaddirEntriesIter::new(raw)?;
                Some(
                    entries
                        .map(|e| DirEntry {
                            name: String::from(e.name),
                            is_dir: e.is_dir,
                        })
                        .collect(),
                )
            }
            _ => None,
        }
    }

    async fn connect(&self, path: &str) -> Result<Arc<dyn Resource>, OpenError> {
        use panda_abi::scheme_protocol::{Request, Response};

        let request_id = self.state.next_request_id();
        let mut req_buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
        let Some(n) = (Request::Connect { request_id, path }).encode(&mut req_buf) else {
            return Err(OpenError::NotFound);
        };
        req_buf.truncate(n);

        let (resp, attachment) = self
            .state
            .round_trip_with_attachment(request_id, &req_buf)
            .await
            .map_err(|_| OpenError::NotFound)?;

        match Response::decode(&resp) {
            Some(Response::ConnectOk { .. }) => {
                // The provider is untrusted input: a `ConnectOk` with no
                // attached channel (bad provider) or an attachment that
                // isn't actually a channel (impossible today — only
                // `ChannelEndpoint` and `SharedBuffer` are transferable at
                // all, per `resource::is_transferable`, but this is still
                // the honest thing to check rather than assume) both fail
                // cleanly rather than handing the caller a resource that
                // doesn't behave like the channel they asked for.
                match attachment {
                    Some(resource) if resource.as_channel().is_some() => Ok(resource),
                    _ => Err(OpenError::NotFound),
                }
            }
            Some(Response::ConnectErr { error, .. }) if error == panda_abi::ErrorCode::Busy => {
                Err(OpenError::Busy)
            }
            _ => Err(OpenError::NotFound),
        }
    }
}

/// A client's open handle to a resource served by a userspace scheme
/// provider. Not VFS-backed — `read`/`write` round-trip to the provider
/// process directly (see `syscall/file.rs`'s scheme-proxy branch).
pub struct SchemeProxyResource {
    provider: Arc<ProviderState>,
    /// Opaque to the kernel: minted by the provider in its `OpenOk` reply
    /// and echoed back on every subsequent request for this resource.
    resource_id: u64,
}

impl SchemeProxyResource {
    /// Read up to `len` bytes (capped by the caller to
    /// `panda_abi::scheme_protocol::MAX_TRANSFER_SIZE`, which is guaranteed
    /// to fit in one response frame).
    pub async fn read(&self, len: usize) -> Result<Vec<u8>, panda_abi::ErrorCode> {
        use panda_abi::scheme_protocol::{Request, Response};

        let request_id = self.provider.next_request_id();
        let mut req_buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
        let Some(n) = (Request::Read {
            request_id,
            resource_id: self.resource_id,
            len: len as u32,
        })
        .encode(&mut req_buf) else {
            return Err(panda_abi::ErrorCode::InvalidArgument);
        };
        req_buf.truncate(n);

        let resp = self
            .provider
            .round_trip(request_id, &req_buf)
            .await
            .map_err(provider_error_to_error_code)?;

        match Response::decode(&resp) {
            Some(Response::ReadOk { data, .. }) => Ok(data.to_vec()),
            Some(Response::ReadErr { error, .. }) => Err(error),
            _ => Err(panda_abi::ErrorCode::Protocol),
        }
    }

    /// Write `data` (capped by the caller to
    /// `panda_abi::scheme_protocol::MAX_TRANSFER_SIZE`).
    pub async fn write(&self, data: &[u8]) -> Result<usize, panda_abi::ErrorCode> {
        use panda_abi::scheme_protocol::{Request, Response};

        let request_id = self.provider.next_request_id();
        let mut req_buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
        let Some(n) = (Request::Write {
            request_id,
            resource_id: self.resource_id,
            data,
        })
        .encode(&mut req_buf) else {
            return Err(panda_abi::ErrorCode::InvalidArgument);
        };
        req_buf.truncate(n);

        let resp = self
            .provider
            .round_trip(request_id, &req_buf)
            .await
            .map_err(provider_error_to_error_code)?;

        match Response::decode(&resp) {
            Some(Response::WriteOk { written, .. }) => Ok(written as usize),
            Some(Response::WriteErr { error, .. }) => Err(error),
            _ => Err(panda_abi::ErrorCode::Protocol),
        }
    }
}

impl Resource for SchemeProxyResource {
    fn handle_type(&self) -> panda_abi::HandleType {
        // Accessed like a file (read/write/close) by the client.
        panda_abi::HandleType::File
    }

    fn as_scheme_proxy(&self) -> Option<&SchemeProxyResource> {
        Some(self)
    }
}

impl Drop for SchemeProxyResource {
    /// Best-effort, fire-and-forget `Close` notification to the provider —
    /// see docs/SYSCALLS.md "Scheme provider operations" for why this
    /// doesn't (and, being a synchronous `Drop`, can't) await the
    /// provider's ack: `ProviderState::round_trip`'s orphan-response
    /// handling makes that safe, discarding this `Close`'s ack if it
    /// arrives ahead of some later, unrelated request's real response.
    ///
    /// Covers both explicit `close()` (dropping the handle-table entry) and
    /// implicit close on process exit (dropping the whole handle table) —
    /// same code path either way, so there's no separate cleanup needed for
    /// the "provider must not leak resources" requirement.
    fn drop(&mut self) {
        use panda_abi::scheme_protocol::Request;

        let request_id = self.provider.next_request_id();
        let mut buf = [0u8; 32];
        if let Some(n) = (Request::Close {
            request_id,
            resource_id: self.resource_id,
        })
        .encode(&mut buf)
        {
            // Ignore send failure: if the provider is gone or its inbound
            // queue is full, there's nothing more we can do from `Drop`.
            let _ = self.provider.kernel_endpoint.send(&buf[..n]);
        }
    }
}

// =============================================================================
// Initialization
// =============================================================================

/// Initialize the resource scheme system with default schemes
pub fn init() {
    register_scheme("file", Arc::new(FileScheme));
    register_scheme("console", Arc::new(ConsoleScheme));
    register_scheme("keyboard", Arc::new(KeyboardScheme));
    register_scheme("display", Arc::new(DisplayScheme));
    register_scheme("block", Arc::new(BlockScheme));
    register_scheme("scheme", Arc::new(SchemeScheme));
}
