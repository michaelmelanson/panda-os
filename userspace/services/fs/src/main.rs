//! The Panda filesystem service (roadmap M5).
//!
//! Registers the `file:` scheme and serves it from the mount table in
//! [`fs::mount`], exactly the interface the kernel's deleted
//! `resource::scheme::FileScheme` used to present. Its own mounts are:
//!
//! - `/initrd` — the kernel's `initrd:` scheme, proxied so
//!   `file:/initrd/...` keeps resolving for existing clients (see
//!   [`fs::schemes::InitrdFs`]).
//! - `/mnt` — ext2 on `block:/pci/storage/0`, i.e. this service is a client
//!   of the userspace virtio-blk driver.
//!
//! # Startup order
//!
//! Mount first, register `file:` second, signal readiness third. That
//! ordering is what replaces the deleted `OP_ENVIRONMENT_MOUNT` syscall:
//! `file:` does not resolve to anything until this process has registered
//! it, and by then both mounts have been attempted, so a client that has
//! seen the readiness handshake (`libpanda::process::wait_ready`, task #43)
//! never observes a half-mounted filesystem. A failed ext2 mount is not
//! fatal — the service still comes up serving `/initrd`, and says so on the
//! log — because several tests run with no block device attached at all.
//!
//! # Blocking serve loop
//!
//! Unlike the drivers in `userspace/drivers/*`, this service blocks in
//! `SchemeProvider::recv` rather than polling with `try_recv` +
//! `yield_now`. Those drivers poll because they have a second event source
//! (their device mailbox) to watch in the same loop; this service has only
//! the provider channel. Spinning would burn CPU on the boot-critical path
//! that every other process's binary is loaded through, which is
//! particularly costly under TCG. The visible consequence is that this
//! service does not implement the drivers' cooperative `SHUTDOWN_REQUEST`
//! convention: it would need a spin loop to notice one, and nothing in the
//! tree sends that message today.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use libpanda::environment;
use libpanda::scheme::SchemeProvider;

use panda_abi::ErrorCode;
use panda_abi::scheme_protocol::{self, ReaddirEntry, Request};

use fs::ext2::Ext2Fs;
use fs::fs::{File, FsError, SeekFrom};
use fs::mount::MountTable;
use fs::schemes::{BLOCK_DEVICE_URI, InitrdFs, SchemeBlockDevice};

/// Where the initrd is mounted, preserving `file:/initrd/...`.
const INITRD_MOUNTPOINT: &str = "/initrd";
/// Where the ext2 root filesystem is mounted, preserving `file:/mnt/...`.
const EXT2_MOUNTPOINT: &str = "/mnt";

/// One resource this service has handed out via `Open` or `Create`.
///
/// A directory has no [`File`] — the protocol has no directory-handle
/// concept, so `Readdir` is answered from the path instead, and reads of a
/// directory fail `IsDirectory` the way they would on any Unix.
struct OpenResource {
    path: String,
    file: Option<Box<dyn File>>,
}

/// The service's whole state: the mount table plus the resources it has
/// handed out.
struct FsService {
    mounts: MountTable,
    open: BTreeMap<u64, OpenResource>,
    next_resource_id: u64,
}

impl FsService {
    fn new(mounts: MountTable) -> Self {
        Self {
            mounts,
            open: BTreeMap::new(),
            // Resource ids are opaque to the kernel; start at 1 so 0 is
            // never a valid id.
            next_resource_id: 1,
        }
    }

    fn insert(&mut self, resource: OpenResource) -> u64 {
        let id = self.next_resource_id;
        self.next_resource_id += 1;
        self.open.insert(id, resource);
        id
    }
}

/// Map an [`FsError`] onto the wire's [`ErrorCode`]. The inverse of the
/// kernel's deleted `syscall::environment::fs_error_code`, which did this
/// same mapping back when the filesystem was kernel-side.
fn error_code(err: FsError) -> ErrorCode {
    match err {
        FsError::NotFound => ErrorCode::NotFound,
        FsError::InvalidOffset => ErrorCode::InvalidOffset,
        FsError::NotReadable => ErrorCode::NotReadable,
        FsError::NotWritable => ErrorCode::NotWritable,
        FsError::NotSeekable => ErrorCode::NotSeekable,
        FsError::ReadOnlyFs => ErrorCode::NotSupported,
        FsError::NoSpace => ErrorCode::NoSpace,
        FsError::AlreadyExists => ErrorCode::AlreadyExists,
        FsError::NotEmpty => ErrorCode::NotEmpty,
        FsError::IsDirectory => ErrorCode::IsDirectory,
        FsError::NotDirectory => ErrorCode::NotDirectory,
        FsError::IoError => ErrorCode::IoError,
    }
}

/// Build the mount table: `/initrd` always, `/mnt` if a block device with a
/// usable ext2 filesystem is there. Returns the table and whether ext2
/// mounted, for the readiness log line.
fn build_mounts() -> (MountTable, bool) {
    let mut mounts = MountTable::new();
    mounts.mount(INITRD_MOUNTPOINT, Arc::new(InitrdFs));

    let device = match SchemeBlockDevice::open(BLOCK_DEVICE_URI) {
        Ok(device) => device,
        Err(_) => {
            environment::log("fs: no block device; serving initrd only");
            return (mounts, false);
        }
    };

    match Ext2Fs::mount(Arc::new(device)) {
        Ok(fs) => {
            mounts.mount(EXT2_MOUNTPOINT, fs);
            environment::log("fs: mounted ext2 at /mnt");
            (mounts, true)
        }
        Err(reason) => {
            environment::log(&alloc::format!("fs: ext2 mount failed: {reason}"));
            (mounts, false)
        }
    }
}

libpanda::main! {
    environment::log("fs: starting");

    let (mounts, _ext2_mounted) = build_mounts();
    let mut service = FsService::new(mounts);

    let Ok(provider) = SchemeProvider::register("file") else {
        environment::log("fs: could not register file: scheme");
        return 1;
    };
    environment::log("fs: registered file: scheme");

    // Readiness handshake: everything this service provides is in place.
    // `init` (and the tests that stand in for it) waits for this before
    // spawning anything that opens `file:`.
    if let Some(parent) = libpanda::ipc::Channel::parent() {
        let _ = parent.send(b"ready");
    }

    let mut buf = [0u8; panda_abi::MAX_MESSAGE_SIZE];
    loop {
        let Ok(request) = provider.recv(&mut buf) else {
            environment::log("fs: provider channel closed, exiting");
            return 0;
        };
        serve(&provider, &mut service, request);
    }
}

/// Answer one request.
fn serve(provider: &SchemeProvider, service: &mut FsService, request: Request<'_>) {
    match request {
        Request::Open { request_id, path } => {
            // Resolve as a directory first, matching what the kernel's
            // `FileScheme::open` did: a `readdir` that succeeds means the
            // path names a directory, and every filesystem here fails
            // `readdir` for a plain file, so one lookup decides which kind
            // of resource this is.
            let resource = if service.mounts.readdir(path).is_ok() {
                OpenResource {
                    path: String::from(path),
                    file: None,
                }
            } else {
                match service.mounts.open(path) {
                    Ok(file) => OpenResource {
                        path: String::from(path),
                        file: Some(file),
                    },
                    Err(err) => {
                        let _ = provider.reply_open_err(request_id, error_code(err));
                        return;
                    }
                }
            };
            let id = service.insert(resource);
            let _ = provider.reply_open_ok(request_id, id);
        }

        Request::Readdir { request_id, path } => match service.mounts.readdir(path) {
            Ok(entries) => {
                let wire: Vec<ReaddirEntry<'_>> = entries
                    .iter()
                    .map(|e| ReaddirEntry {
                        name: e.name.as_str(),
                        is_dir: e.is_dir,
                    })
                    .collect();
                if provider.reply_readdir_ok(request_id, &wire).is_err() {
                    // The only way this fails is the listing not fitting in
                    // one frame (pagination is a documented protocol
                    // follow-up, not solved here) — say so rather than
                    // leaving the caller waiting.
                    let _ = provider.reply_readdir_err(request_id, ErrorCode::MessageTooLarge);
                }
            }
            Err(err) => {
                let _ = provider.reply_readdir_err(request_id, error_code(err));
            }
        },

        Request::Read {
            request_id,
            resource_id,
            offset,
            len,
            ..
        } => {
            let len = (len as usize).min(scheme_protocol::MAX_TRANSFER_SIZE);
            let Some(resource) = service.open.get_mut(&resource_id) else {
                let _ = provider.reply_read_err(request_id, ErrorCode::InvalidHandle);
                return;
            };
            let Some(file) = resource.file.as_mut() else {
                let _ = provider.reply_read_err(request_id, ErrorCode::IsDirectory);
                return;
            };
            if len == 0 {
                let _ = provider.reply_read_ok(request_id, &[]);
                return;
            }
            if let Err(err) = file.seek(SeekFrom::Start(offset)) {
                let _ = provider.reply_read_err(request_id, error_code(err));
                return;
            }
            let mut data = alloc::vec![0u8; len];
            match file.read(&mut data) {
                Ok(n) => {
                    data.truncate(n);
                    let _ = provider.reply_read_ok(request_id, &data);
                }
                Err(err) => {
                    let _ = provider.reply_read_err(request_id, error_code(err));
                }
            }
        }

        Request::Write {
            request_id,
            resource_id,
            offset,
            data,
        } => {
            let Some(resource) = service.open.get_mut(&resource_id) else {
                let _ = provider.reply_write_err(request_id, ErrorCode::InvalidHandle);
                return;
            };
            let Some(file) = resource.file.as_mut() else {
                let _ = provider.reply_write_err(request_id, ErrorCode::IsDirectory);
                return;
            };
            if let Err(err) = file.seek(SeekFrom::Start(offset)) {
                let _ = provider.reply_write_err(request_id, error_code(err));
                return;
            }
            match file.write(data) {
                Ok(n) => {
                    let _ = provider.reply_write_ok(request_id, n as u32);
                }
                Err(err) => {
                    let _ = provider.reply_write_err(request_id, error_code(err));
                }
            }
        }

        Request::Size {
            request_id,
            resource_id,
        } => match stat_resource(service, resource_id) {
            Ok((size, _is_dir)) => {
                let _ = provider.reply_size_ok(request_id, size);
            }
            Err(code) => {
                let _ = provider.reply_size_err(request_id, code);
            }
        },

        Request::Stat {
            request_id,
            resource_id,
        } => match stat_resource(service, resource_id) {
            Ok((size, is_dir)) => {
                let _ = provider.reply_stat_ok(request_id, size, is_dir);
            }
            Err(code) => {
                let _ = provider.reply_stat_err(request_id, code);
            }
        },

        Request::Create {
            request_id,
            mode,
            path,
        } => match service.mounts.create(path, mode) {
            Ok(file) => {
                let id = service.insert(OpenResource {
                    path: String::from(path),
                    file: Some(file),
                });
                let _ = provider.reply_create_ok(request_id, id);
            }
            Err(err) => {
                let _ = provider.reply_create_err(request_id, error_code(err));
            }
        },

        Request::Unlink { request_id, path } => {
            reply_path_op(
                provider,
                scheme_protocol::MSG_UNLINK,
                request_id,
                service.mounts.unlink(path),
            );
        }

        Request::Mkdir {
            request_id,
            mode,
            path,
        } => {
            reply_path_op(
                provider,
                scheme_protocol::MSG_MKDIR,
                request_id,
                service.mounts.mkdir(path, mode),
            );
        }

        Request::Rmdir { request_id, path } => {
            reply_path_op(
                provider,
                scheme_protocol::MSG_RMDIR,
                request_id,
                service.mounts.rmdir(path),
            );
        }

        Request::Close {
            request_id,
            resource_id,
        } => {
            service.open.remove(&resource_id);
            let _ = provider.reply_close_ok(request_id);
        }

        // `file:` has no connect-style protocol and serves no display.
        Request::Connect { request_id, .. } => {
            let _ = provider.reply_connect_err(request_id, ErrorCode::NotSupported);
        }
        Request::DisplayInfo { request_id, .. } => {
            let _ = provider.reply_display_info_err(request_id, ErrorCode::NotSupported);
        }
        Request::DisplayMap { request_id, .. } => {
            let _ = provider.reply_display_map_err(request_id, ErrorCode::NotSupported);
        }
        Request::DisplayFlush { request_id, .. } => {
            let _ = provider.reply_display_flush_err(request_id, ErrorCode::NotSupported);
        }
    }
}

/// `(size, is_dir)` for an open resource, shared by `Size` and `Stat`.
fn stat_resource(service: &FsService, resource_id: u64) -> Result<(u64, bool), ErrorCode> {
    let resource = service
        .open
        .get(&resource_id)
        .ok_or(ErrorCode::InvalidHandle)?;
    match resource.file.as_ref() {
        Some(file) => {
            let stat = file.stat().map_err(error_code)?;
            Ok((stat.size, stat.is_dir))
        }
        // A directory resource has no `File` to ask, so go back to the
        // mount table with the path it was opened from.
        None => {
            let stat = service.mounts.stat(&resource.path).map_err(error_code)?;
            Ok((stat.size, stat.is_dir))
        }
    }
}

/// Reply to one of the three payload-free path operations.
fn reply_path_op(
    provider: &SchemeProvider,
    kind: u8,
    request_id: u64,
    result: Result<(), FsError>,
) {
    match result {
        Ok(()) => {
            let _ = provider.reply_path_op_ok(kind, request_id);
        }
        Err(err) => {
            let _ = provider.reply_path_op_err(kind, request_id, error_code(err));
        }
    }
}
