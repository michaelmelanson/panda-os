//! Resource scheme system for unified resource access.
//!
//! Resources are identified by URIs with a scheme and path:
//! - `initrd:/init` -> File in the in-memory boot archive (kernel-served)
//! - `file:/mnt/x` -> File served by the userspace filesystem service
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

use crate::vfs;

use super::Resource;
use super::MailboxRef;
use super::char_output::{CharOutError, CharacterOutput};
use super::directory::{DirEntry, Directory};

/// Error returned when opening a resource via a scheme fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// No resource exists at the given URI.
    NotFound,
    /// The resource exists but is exclusively claimed by another owner
    /// (for a provider-backed scheme, that means the provider answered its
    /// `Open` with `Busy` — see `userspace/drivers/virtio-blk`).
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

    /// Create a file at `path` within this scheme, returning it open.
    ///
    /// Backs `OP_DIRECTORY_CREATE` (see `syscall::directory::handle_create`).
    /// Roadmap M5: this used to call `vfs::create` directly in the kernel;
    /// now the filesystem lives in `userspace/services/fs`, so it has to be
    /// a request to whichever provider owns the scheme. Only
    /// [`UserSchemeProvider`] implements it — everything else inherits this
    /// default, which is also the honest answer for `initrd:` (read-only)
    /// and the device schemes.
    async fn create(&self, _path: &str, _mode: u16) -> Result<Box<dyn Resource>, panda_abi::ErrorCode> {
        Err(panda_abi::ErrorCode::NotSupported)
    }

    /// Remove the file at `path` within this scheme. See [`create`](Self::create).
    async fn unlink(&self, _path: &str) -> Result<(), panda_abi::ErrorCode> {
        Err(panda_abi::ErrorCode::NotSupported)
    }

    /// Create a directory at `path` within this scheme. See [`create`](Self::create).
    async fn mkdir(&self, _path: &str, _mode: u16) -> Result<(), panda_abi::ErrorCode> {
        Err(panda_abi::ErrorCode::NotSupported)
    }

    /// Remove the empty directory at `path` within this scheme. See
    /// [`create`](Self::create).
    async fn rmdir(&self, _path: &str) -> Result<(), panda_abi::ErrorCode> {
        Err(panda_abi::ErrorCode::NotSupported)
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

/// Resolve `uri`'s scheme to its handler, or `NotFound` if nothing is
/// registered under that name. Shared by the four path-based filesystem
/// operations below.
fn handler_for(uri: &str) -> Result<(Arc<dyn SchemeHandler>, &str), panda_abi::ErrorCode> {
    let (scheme, path) = uri.split_once(':').ok_or(panda_abi::ErrorCode::NotFound)?;
    let schemes = SCHEMES.read();
    let handler = schemes
        .get(scheme)
        .map(Arc::clone)
        .ok_or(panda_abi::ErrorCode::NotFound)?;
    Ok((handler, path))
}

/// Create a file by URI (e.g. `"file:/mnt/new.txt"`), returning it open.
/// See [`SchemeHandler::create`].
pub async fn create(uri: &str, mode: u16) -> Result<Box<dyn Resource>, panda_abi::ErrorCode> {
    let (handler, path) = handler_for(uri)?;
    handler.create(path, mode).await
}

/// Remove a file by URI. See [`SchemeHandler::create`].
pub async fn unlink(uri: &str) -> Result<(), panda_abi::ErrorCode> {
    let (handler, path) = handler_for(uri)?;
    handler.unlink(path).await
}

/// Create a directory by URI. See [`SchemeHandler::create`].
pub async fn mkdir(uri: &str, mode: u16) -> Result<(), panda_abi::ErrorCode> {
    let (handler, path) = handler_for(uri)?;
    handler.mkdir(path, mode).await
}

/// Remove an empty directory by URI. See [`SchemeHandler::create`].
pub async fn rmdir(uri: &str) -> Result<(), panda_abi::ErrorCode> {
    let (handler, path) = handler_for(uri)?;
    handler.rmdir(path).await
}

// =============================================================================
// File Scheme - moved to a userspace service in roadmap M5
// =============================================================================
//
// The kernel-resident `FileScheme` (which walked `vfs`'s mount table) has
// been deleted along with the mount table and ext2 themselves. `file:` is
// now registered by `userspace/services/fs` via `OP_SCHEME_REGISTER`,
// exactly like `block:`/`display:`/`keyboard:`, and resolves through
// `UserSchemeProvider` below with no special-casing here. The kernel keeps
// only `initrd:` (see `resource::initrd::InitrdScheme`), which reads the
// in-memory ustar archive directly and is what makes the filesystem service
// itself loadable before any filesystem is mounted.

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

/// A directory resource: a snapshot of one directory's entries, plus the URI
/// it was listed from.
///
/// The URI is what makes the mutating directory syscalls
/// (`OP_DIRECTORY_CREATE`/`UNLINK`/`MKDIR`/`RMDIR`) possible: they join it
/// with the caller's entry name and dispatch the result back through the
/// scheme registry (see [`create`]/[`unlink`]/[`mkdir`]/[`rmdir`] and
/// `syscall::directory`). Before roadmap M5 this was a bare VFS path,
/// because the only mutable filesystem was the kernel's own; it is a full
/// URI now so the operation reaches whichever provider owns that scheme.
pub struct DirectoryResource {
    entries: Vec<DirEntry>,
    /// The URI this listing came from (e.g. `"file:/mnt"`), if it supports
    /// mutation. `None` for listings with no mutable backing (e.g.
    /// `initrd:`, `scheme:`).
    uri: Option<alloc::string::String>,
}

impl DirectoryResource {
    pub fn new(entries: Vec<DirEntry>) -> Self {
        Self { entries, uri: None }
    }

    /// Create a directory resource that supports mutation, tagged with the
    /// URI it was listed from.
    pub fn with_uri(entries: Vec<DirEntry>, uri: alloc::string::String) -> Self {
        Self {
            entries,
            uri: Some(uri),
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

    fn uri(&self) -> Option<alloc::string::String> {
        self.uri.clone()
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
// Display Scheme - moved to a userspace driver in roadmap M4
// =============================================================================
//
// The kernel-resident `DisplayScheme`/`DisplayDevice` (`display:
// /pci/display/0`, with a kernel-side exclusive claim) has been
// deleted: the display driver is now a userspace process
// (`userspace/drivers/virtio-gpu`) that registers `display:` itself via
// `OP_SCHEME_REGISTER`, exactly like `userspace/drivers/virtio-keyboard`
// registers `keyboard:`. `OP_DISPLAY_INFO`/`MAP`/`FLUSH` (the client-facing
// contract, unchanged) are served for a `SchemeProxyResource`-backed
// `display:` open by forwarding to the provider — see
// `SchemeProxyResource::display_info`/`display_map`/`display_flush` above
// and `panda-kernel/src/syscall/display.rs`. Exclusivity (a second open
// failing `Busy`) is now the provider's responsibility, not a kernel claim —
// see the virtio-gpu driver's `Open` handler.

// =============================================================================
// Block Scheme - moved to userspace (M4 Phase 2, plans/device-driver-model.md)
// =============================================================================
//
// `block:` is no longer a kernel-native scheme handler. The userspace
// `virtio-blk` driver (userspace/drivers/virtio-blk) registers it via
// `OP_SCHEME_REGISTER`/`register_user_scheme`, exactly like `keyboard:` —
// `resource::open("block:/...")` resolves through `UserSchemeProvider`
// (registered below by whichever driver claims the block device) with no
// special-casing needed here. Exclusivity (the M1 claim rules `block:` must
// honour — see `claim_test`) is enforced by that driver's own `Open`
// handling: an open *is* the claim, and closing it releases it, which is why
// the kernel's old `devices::claims` table has no users left and was
// deleted. Its one kernel-internal client, the ext2 mount, is now a
// userspace process too (`userspace/services/fs`), so the kernel-side
// `SchemeBlockDevice` adapter that used to bridge the two went with it.

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

/// One in-flight `round_trip` call's mailbox: the background reader task
/// (see [`ProviderState::spawn_reader`]) fills in `result` and wakes
/// `waker` once it reads the matching response frame off the wire.
struct PendingEntry {
    result: Option<Result<(Vec<u8>, Option<Arc<dyn Resource>>), ProviderError>>,
    waker: Option<core::task::Waker>,
}

/// Shared state for one registered userspace scheme provider.
///
/// Request/response correlation and unsolicited event delivery
/// (`Response::Notify` — see `panda_abi::scheme_protocol::MSG_NOTIFY`) both
/// need *something* continuously draining `kernel_endpoint`, independent of
/// whether any client currently has a round trip in flight — otherwise a
/// `Notify` sent while no client is blocked in `round_trip` (e.g. a client
/// that already opened a resource and is now just waiting on its mailbox
/// for `EVENT_KEYBOARD_KEY`-style notifications, per
/// `mailbox_keyboard_test`) would sit unread forever. `spawn_reader` is
/// that something: one persistent kernel background task
/// (`crate::executor::spawn`) per provider, spawned once in [`Self::new`],
/// that owns every read off the channel for the provider's whole lifetime.
/// `round_trip`/`round_trip_with_attachment` never read the channel
/// themselves — they register a [`PendingEntry`] keyed by `request_id` and
/// await it being filled in.
///
/// This also lifts the old v1 "single request in flight per provider"
/// restriction: since responses are now correlated by `request_id` via
/// `pending` rather than "whichever response happens to arrive next",
/// concurrent round trips to the same provider are naturally multiplexed
/// instead of queueing behind each other.
struct ProviderState {
    /// The kernel's endpoint of the channel; the provider process holds the
    /// other endpoint and serves requests with the ordinary
    /// `OP_CHANNEL_SEND`/`OP_CHANNEL_RECV` syscalls it already has.
    kernel_endpoint: super::ChannelEndpoint,
    next_request_id: core::sync::atomic::AtomicU64,
    /// Round trips awaiting a response, keyed by `request_id`.
    pending: Spinlock<BTreeMap<u64, PendingEntry>>,
    /// Mailboxes to notify (`Response::Notify`) for a given `resource_id`,
    /// registered via `Resource::attach_mailbox` on the `SchemeProxyResource`
    /// the client got back from `Open`.
    mailboxes: Spinlock<BTreeMap<u64, Vec<MailboxRef>>>,
    /// Set once [`spawn_reader`](Self::spawn_reader)'s background task sees
    /// the provider's channel disconnect, right before that task returns
    /// (permanently — see its doc comment). Without this, a round trip
    /// started *after* the reader task has already exited has nothing to
    /// ever fail it: the task's own disconnect handling only wakes
    /// round trips already in `pending` *at the moment it observes the
    /// disconnect*, and once it returns, nothing will ever read the
    /// channel, dispatch a response, or run that sweep again. Every new
    /// `round_trip_with_attachment` call checks this flag itself so it
    /// fails immediately instead of registering a pending entry no one is
    /// left to resolve — found via `scheme_provider_test`'s
    /// provider-already-exited case hanging instead of erroring cleanly.
    disconnected: core::sync::atomic::AtomicBool,
}

impl ProviderState {
    /// Construct the shared state and immediately spawn its background
    /// reader task (see the struct doc comment) — an `Arc` because the
    /// spawned task needs its own strong reference independent of every
    /// `SchemeProxyResource`/`UserSchemeProvider` that also holds one.
    fn new(kernel_endpoint: super::ChannelEndpoint) -> Arc<Self> {
        let state = Arc::new(Self {
            kernel_endpoint,
            next_request_id: core::sync::atomic::AtomicU64::new(1),
            pending: Spinlock::new(BTreeMap::new()),
            mailboxes: Spinlock::new(BTreeMap::new()),
            disconnected: core::sync::atomic::AtomicBool::new(false),
        });
        Self::spawn_reader(state.clone());
        state
    }

    fn next_request_id(&self) -> u64 {
        self.next_request_id
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
    }

    /// Register `mailbox_ref` to receive `Response::Notify` events posted
    /// for `resource_id`. See `SchemeProxyResource::attach_mailbox`.
    fn attach_mailbox(&self, resource_id: u64, mailbox_ref: MailboxRef) {
        self.mailboxes
            .lock()
            .entry(resource_id)
            .or_default()
            .push(mailbox_ref);
    }

    fn dispatch_notify(&self, resource_id: u64, events: u32) {
        if let Some(refs) = self.mailboxes.lock().get(&resource_id) {
            for mailbox_ref in refs {
                mailbox_ref.post_event(events);
            }
        }
    }

    /// The persistent background task described on the struct doc comment.
    /// Runs for the provider's whole lifetime (until its channel's peer
    /// closes), reading one frame at a time and either routing it to the
    /// `round_trip` it answers (by `request_id`) or, for an unsolicited
    /// `Response::Notify` (`request_id == 0`), dispatching it to attached
    /// mailboxes.
    fn spawn_reader(state: Arc<Self>) {
        crate::executor::spawn(async move {
            loop {
                let mut buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
                let outcome = core::future::poll_fn(|_cx| {
                    let mut registered = false;
                    loop {
                        match state.kernel_endpoint.recv_with_attachment(&mut buf) {
                            Ok((len, attachment)) => {
                                return core::task::Poll::Ready(Some((len, attachment)));
                            }
                            Err(super::ChannelError::QueueEmpty) => {
                                if registered {
                                    return core::task::Poll::Pending;
                                }
                                // Register, then retry immediately: a frame
                                // may arrive (and call `wake()`) between the
                                // `recv` above and this registration, in
                                // which case `wake()` would find no
                                // registered waiter and this task would
                                // never be woken to read it.
                                let waker = state.kernel_endpoint.waker();
                                if let Some(task_id) = crate::executor::current_task_id() {
                                    waker.set_waiting_task(task_id);
                                }
                                registered = true;
                            }
                            // Peer closed, or a malformed length: either
                            // way, nothing more will ever arrive on this
                            // channel — stop reading.
                            Err(_) => return core::task::Poll::Ready(None),
                        }
                    }
                })
                .await;

                let Some((len, attachment)) = outcome else {
                    // The provider disconnected. Fail every round trip
                    // still waiting rather than leaving callers blocked
                    // forever, set `disconnected` so every *future* round
                    // trip fails immediately too (see that field's doc
                    // comment), then stop: no more frames will ever arrive.
                    let mut pending = state.pending.lock();
                    for entry in pending.values_mut() {
                        entry.result = Some(Err(ProviderError::Disconnected));
                        if let Some(waker) = entry.waker.take() {
                            waker.wake();
                        }
                    }
                    drop(pending);
                    state.disconnected.store(true, core::sync::atomic::Ordering::Release);
                    return;
                };
                buf.truncate(len);

                if buf.len() < 9 {
                    // Malformed frame: not attributable to any pending
                    // round trip, so there's nothing to do but drop it and
                    // keep reading.
                    continue;
                }
                let request_id = u64::from_le_bytes(buf[1..9].try_into().unwrap());

                if request_id == 0 {
                    // Unsolicited push (`Response::Notify`) — see
                    // `panda_abi::scheme_protocol::MSG_NOTIFY`.
                    if let Some(panda_abi::scheme_protocol::Response::Notify {
                        resource_id,
                        events,
                    }) = panda_abi::scheme_protocol::Response::decode(&buf)
                    {
                        state.dispatch_notify(resource_id, events);
                    }
                    continue;
                }

                let mut pending = state.pending.lock();
                if let Some(entry) = pending.get_mut(&request_id) {
                    entry.result = Some(Ok((buf, attachment)));
                    if let Some(waker) = entry.waker.take() {
                        waker.wake();
                    }
                }
                // Else: an orphaned response (e.g. a fire-and-forget
                // `Close`'s ack — see `SchemeProxyResource::drop` — whose
                // caller already stopped waiting). Nothing to deliver it to;
                // drop it and keep reading.
            }
        });
    }

    /// Send `request` and wait for the matching response.
    ///
    /// `request_id` is threaded through the wire format both to correlate
    /// concurrent round trips (see the struct doc comment) and to pay for
    /// itself in exactly the situation handled by [`spawn_reader`]'s
    /// orphaned-response case: a fire-and-forget `Close` (see
    /// `SchemeProxyResource`'s `Drop`) can leave an orphaned response with
    /// no matching `pending` entry, which is silently dropped there instead
    /// of being misdelivered to some later, unrelated caller.
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
        // Fast path: the reader task already saw the provider disconnect
        // (and, per `disconnected`'s doc comment, has permanently stopped
        // running, so nothing will ever resolve a pending entry for us) —
        // fail immediately instead of registering one.
        if self.disconnected.load(core::sync::atomic::Ordering::Acquire) {
            return Err(ProviderError::Disconnected);
        }

        self.pending.lock().insert(
            request_id,
            PendingEntry {
                result: None,
                waker: None,
            },
        );

        if self.kernel_endpoint.send(request).is_err() {
            self.pending.lock().remove(&request_id);
            return Err(ProviderError::Disconnected);
        }

        core::future::poll_fn(|cx| {
            let mut pending = self.pending.lock();
            let Some(entry) = pending.get_mut(&request_id) else {
                // Shouldn't happen (only this call removes its own entry,
                // and only after taking its result) but fail safe rather
                // than panic if it ever does.
                return core::task::Poll::Ready(Err(ProviderError::Disconnected));
            };
            match entry.result.take() {
                Some(result) => {
                    pending.remove(&request_id);
                    core::task::Poll::Ready(result)
                }
                None => {
                    // Re-check disconnection here too: the reader task may
                    // have set the flag and swept `pending` *between* our
                    // insert above and this poll — if this call raced the
                    // insert itself (our entry wasn't in `pending` yet when
                    // the sweep ran), it never got woken at all and would
                    // otherwise wait forever for a wake that already
                    // happened.
                    if self.disconnected.load(core::sync::atomic::Ordering::Acquire) {
                        return core::task::Poll::Ready(Err(ProviderError::Disconnected));
                    }
                    entry.waker = Some(cx.waker().clone());
                    core::task::Poll::Pending
                }
            }
        })
        .await
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
            state: ProviderState::new(kernel_endpoint),
        }
    }

    /// Round-trip one of the payload-free path operations
    /// (`Unlink`/`Mkdir`/`Rmdir` — see
    /// `panda_abi::scheme_protocol::MSG_CREATE`). They share one request
    /// shape and one payload-free response, so `kind` is all that
    /// distinguishes them; `mode` is ignored by everything but `Mkdir`.
    async fn path_op(
        &self,
        kind: u8,
        mode: u16,
        path: &str,
    ) -> Result<(), panda_abi::ErrorCode> {
        use panda_abi::scheme_protocol::{self, Request, Response};

        let request_id = self.state.next_request_id();
        let request = match kind {
            scheme_protocol::MSG_UNLINK => Request::Unlink { request_id, path },
            scheme_protocol::MSG_MKDIR => Request::Mkdir { request_id, mode, path },
            scheme_protocol::MSG_RMDIR => Request::Rmdir { request_id, path },
            _ => return Err(panda_abi::ErrorCode::InvalidArgument),
        };

        let mut req_buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
        let Some(n) = request.encode(&mut req_buf) else {
            return Err(panda_abi::ErrorCode::InvalidArgument);
        };
        req_buf.truncate(n);

        let resp = self
            .state
            .round_trip(request_id, &req_buf)
            .await
            .map_err(provider_error_to_error_code)?;

        match Response::decode(&resp) {
            Some(Response::PathOpOk { .. }) => Ok(()),
            Some(Response::PathOpErr { error, .. }) => Err(error),
            _ => Err(panda_abi::ErrorCode::Protocol),
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

    async fn create(&self, path: &str, mode: u16) -> Result<Box<dyn Resource>, panda_abi::ErrorCode> {
        use panda_abi::scheme_protocol::{Request, Response};

        let request_id = self.state.next_request_id();
        let mut req_buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
        let Some(n) = (Request::Create { request_id, mode, path }).encode(&mut req_buf) else {
            return Err(panda_abi::ErrorCode::InvalidArgument);
        };
        req_buf.truncate(n);

        let resp = self
            .state
            .round_trip(request_id, &req_buf)
            .await
            .map_err(provider_error_to_error_code)?;

        match Response::decode(&resp) {
            Some(Response::CreateOk { resource_id, .. }) => Ok(Box::new(SchemeProxyResource {
                provider: Arc::clone(&self.state),
                resource_id,
            })),
            Some(Response::CreateErr { error, .. }) => Err(error),
            _ => Err(panda_abi::ErrorCode::Protocol),
        }
    }

    async fn unlink(&self, path: &str) -> Result<(), panda_abi::ErrorCode> {
        self.path_op(panda_abi::scheme_protocol::MSG_UNLINK, 0, path)
            .await
    }

    async fn mkdir(&self, path: &str, mode: u16) -> Result<(), panda_abi::ErrorCode> {
        self.path_op(panda_abi::scheme_protocol::MSG_MKDIR, mode, path)
            .await
    }

    async fn rmdir(&self, path: &str) -> Result<(), panda_abi::ErrorCode> {
        self.path_op(panda_abi::scheme_protocol::MSG_RMDIR, 0, path)
            .await
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
    ///
    /// `nonblocking` is forwarded to the provider as `Request::Read`'s
    /// `nonblocking` field — see that field's doc comment. This method
    /// itself still always awaits one full round trip either way; what
    /// changes is whether the *provider* is allowed to defer its reply
    /// until data shows up (blocking) or must answer immediately, even
    /// with zero bytes, if none is buffered yet (non-blocking). Without
    /// this, `OP_FILE_READ`'s `FILE_NONBLOCK` flag had no effect at all for
    /// scheme-proxy resources — a provider that defers empty reads (the
    /// natural way to give *blocking* reads real semantics over a
    /// request/response protocol, e.g. `userspace/drivers/virtio-keyboard`)
    /// made every read through it block, non-blocking or not, which hung
    /// `mailbox_keyboard_test`'s non-blocking drain loop forever once it
    /// caught up with the buffered events. See
    /// `plans/device-driver-model.md`'s Phase 7 landed-deviations note.
    pub async fn read(&self, len: usize, nonblocking: bool) -> Result<Vec<u8>, panda_abi::ErrorCode> {
        self.read_at(0, len, nonblocking).await
    }

    /// As [`read`](Self::read), but at a specific byte `offset` — used by
    /// the filesystem service's `block:` client (`userspace/services/fs`)
    /// for random-access reads. Stream-like providers (the only
    /// kind [`read`](Self::read) itself is used against today, e.g.
    /// `keyboard:`) are free to ignore `offset` — see `Request::Read`'s doc
    /// comment.
    pub async fn read_at(&self, offset: u64, len: usize, nonblocking: bool) -> Result<Vec<u8>, panda_abi::ErrorCode> {
        use panda_abi::scheme_protocol::{Request, Response};

        let request_id = self.provider.next_request_id();
        let mut req_buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
        let Some(n) = (Request::Read {
            request_id,
            resource_id: self.resource_id,
            offset,
            len: len as u32,
            nonblocking,
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
        self.write_at(0, data).await
    }

    /// As [`write`](Self::write), but at a specific byte `offset` — see
    /// [`read_at`](Self::read_at)'s doc comment.
    pub async fn write_at(&self, offset: u64, data: &[u8]) -> Result<usize, panda_abi::ErrorCode> {
        use panda_abi::scheme_protocol::{Request, Response};

        let request_id = self.provider.next_request_id();
        let mut req_buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
        let Some(n) = (Request::Write {
            request_id,
            resource_id: self.resource_id,
            offset,
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

    /// Query the resource's byte size — see `Request::Size`'s doc comment.
    /// Used by the filesystem service's `block:` client once, at open
    /// time, to populate its synchronous `BlockDevice::size()` accessor
    /// (the wire protocol only supports async round trips, so it can't be
    /// queried per-call).
    pub async fn size(&self) -> Result<u64, panda_abi::ErrorCode> {
        use panda_abi::scheme_protocol::{Request, Response};

        let request_id = self.provider.next_request_id();
        let mut req_buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
        let Some(n) = (Request::Size {
            request_id,
            resource_id: self.resource_id,
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
            Some(Response::SizeOk { size, .. }) => Ok(size),
            Some(Response::SizeErr { error, .. }) => Err(error),
            _ => Err(panda_abi::ErrorCode::Protocol),
        }
    }

    /// Query the resource's metadata, returning `(size, is_dir)` — see
    /// `Request::Stat`'s doc comment. Backs `OP_FILE_STAT` for a
    /// provider-backed resource (roadmap M5, when `file:` became a
    /// userspace scheme); distinct from [`size`](Self::size), which predates
    /// it and deliberately answers only a byte count.
    pub async fn stat(&self) -> Result<(u64, bool), panda_abi::ErrorCode> {
        use panda_abi::scheme_protocol::{Request, Response};

        let request_id = self.provider.next_request_id();
        let mut req_buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
        let Some(n) = (Request::Stat {
            request_id,
            resource_id: self.resource_id,
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
            Some(Response::StatOk { size, is_dir, .. }) => Ok((size, is_dir)),
            Some(Response::StatErr { error, .. }) => Err(error),
            _ => Err(panda_abi::ErrorCode::Protocol),
        }
    }

    /// Forward `OP_DISPLAY_INFO` to the provider — see
    /// `panda-kernel/src/syscall/display.rs`'s scheme-proxy fallback branch,
    /// used when the opened resource is a userspace display driver
    /// (roadmap M4) rather than the old kernel-resident `DisplayDevice`.
    /// Returns `(width, height, format, stride)`.
    pub async fn display_info(&self) -> Result<(u32, u32, u32, u32), panda_abi::ErrorCode> {
        use panda_abi::scheme_protocol::{Request, Response};

        let request_id = self.provider.next_request_id();
        let mut req_buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
        let Some(n) = (Request::DisplayInfo {
            request_id,
            resource_id: self.resource_id,
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
            Some(Response::DisplayInfoOk { width, height, format, stride, .. }) => {
                Ok((width, height, format, stride))
            }
            Some(Response::DisplayInfoErr { error, .. }) => Err(error),
            _ => Err(panda_abi::ErrorCode::Protocol),
        }
    }

    /// Forward `OP_DISPLAY_MAP` to the provider. Returns the framebuffer's
    /// guest-physical address and byte size; the syscall handler maps that
    /// range into the calling process (see `MSG_DISPLAY_MAP`'s doc comment
    /// for why the provider discloses a raw physical range here).
    pub async fn display_map(&self) -> Result<(u64, u64), panda_abi::ErrorCode> {
        use panda_abi::scheme_protocol::{Request, Response};

        let request_id = self.provider.next_request_id();
        let mut req_buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
        let Some(n) = (Request::DisplayMap {
            request_id,
            resource_id: self.resource_id,
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
            Some(Response::DisplayMapOk { phys_addr, size, .. }) => Ok((phys_addr, size)),
            Some(Response::DisplayMapErr { error, .. }) => Err(error),
            _ => Err(panda_abi::ErrorCode::Protocol),
        }
    }

    /// Forward `OP_DISPLAY_FLUSH` to the provider. `rect` is `None` for a
    /// full-surface flush.
    pub async fn display_flush(
        &self,
        rect: Option<(u32, u32, u32, u32)>,
    ) -> Result<(), panda_abi::ErrorCode> {
        use panda_abi::scheme_protocol::{Request, Response};

        let request_id = self.provider.next_request_id();
        let mut req_buf = alloc::vec![0u8; panda_abi::MAX_MESSAGE_SIZE];
        let Some(n) = (Request::DisplayFlush {
            request_id,
            resource_id: self.resource_id,
            rect,
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
            Some(Response::DisplayFlushOk { .. }) => Ok(()),
            Some(Response::DisplayFlushErr { error, .. }) => Err(error),
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

    /// Registers `mailbox_ref` to receive `Response::Notify` pushes the
    /// provider sends for this resource (see
    /// `panda_abi::scheme_protocol::MSG_NOTIFY` and
    /// `ProviderState::spawn_reader`) — e.g. the userspace virtio-keyboard
    /// driver notifying `EVENT_KEYBOARD_KEY` when it buffers a new key
    /// event, for a client that opened `keyboard:` with a mailbox attached
    /// (`OP_ENVIRONMENT_OPEN`'s `mailbox`/`event_mask` args, as
    /// `mailbox_keyboard_test` does).
    fn attach_mailbox(&self, mailbox_ref: super::MailboxRef) {
        self.provider.attach_mailbox(self.resource_id, mailbox_ref);
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

/// Register the `initrd:` scheme, backed directly by the in-memory ustar
/// archive rather than the general VFS mount table. Called separately from
/// [`init`] because it needs the already-parsed [`vfs::TarFs`] the kernel
/// built to mount `/initrd` — see `panda-kernel/src/main.rs`.
pub fn register_initrd_scheme(fs: Arc<vfs::TarFs>) {
    register_scheme("initrd", Arc::new(super::initrd::InitrdScheme::new(fs)));
}

/// Initialize the resource scheme system with default schemes
pub fn init() {
    register_scheme("console", Arc::new(ConsoleScheme));
    // "file", "block" and "display" are intentionally not registered here —
    // each is registered by its userspace provider via `OP_SCHEME_REGISTER`,
    // same as "keyboard": "file" by `userspace/services/fs` (roadmap M5),
    // "block"/"display" by `userspace/drivers/virtio-blk`/`virtio-gpu`
    // (roadmap M4).
    register_scheme("scheme", Arc::new(SchemeScheme));
}
