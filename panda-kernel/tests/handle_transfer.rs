//! Tests for channel handle transfer (SCM_RIGHTS-style attachment).
//!
//! These exercise the resource-layer primitives added for handle transfer —
//! `ChannelEndpoint::send_with_attachment`/`recv_with_attachment`/
//! `peek_has_attachment`, `resource::is_transferable`, and
//! `HandleTable::is_full` — directly. The orchestration that ties them
//! together (whitelist enforcement on send, installing the attachment into
//! the receiver's handle table on recv) lives in the syscall handler
//! (`syscall/channel.rs`), which isn't reachable from a kernel-only
//! integration test since it needs a running process/scheduler. The
//! userspace test `handle_transfer_test` exercises that full syscall/ABI
//! path end-to-end.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::sync::Arc;
use alloc::vec::Vec;

use panda_kernel::handle::{HandleTable, MAX_HANDLES_PER_PROCESS};
use panda_kernel::resource::{ChannelEndpoint, DirectoryResource, Resource, is_transferable};

panda_kernel::test_harness!(
    attachment_installs_channel_handle_on_receiver,
    non_whitelisted_resource_is_rejected_by_whitelist,
    message_without_attachment_reports_none,
    recv_with_full_handle_table_leaves_message_queued,
    endpoint_can_be_sent_through_itself,
);

/// Sending an endpoint of a second channel pair through a first pair
/// installs a channel-typed handle on the receiving side.
fn attachment_installs_channel_handle_on_receiver() {
    // Pair 1 carries the message; pair 2's endpoint B is what gets
    // transferred.
    let (a1, b1) = ChannelEndpoint::create_pair();
    let (_a2, b2) = ChannelEndpoint::create_pair();

    let b2_resource: Arc<dyn Resource> = Arc::new(b2);
    assert!(
        is_transferable(&b2_resource),
        "a plain channel endpoint should be whitelisted for transfer"
    );

    a1.send_with_attachment(b"hello", Some(b2_resource))
        .expect("send with attachment should succeed");

    let mut buf = [0u8; 64];
    let (len, attachment) = b1
        .recv_with_attachment(&mut buf)
        .expect("recv should succeed");
    assert_eq!(&buf[..len], b"hello");

    let attachment = attachment.expect("message should carry an attachment");

    // Install it into a handle table, exactly as the recv syscall handler
    // does, and verify the installed handle's resource is a channel.
    let mut table = HandleTable::new();
    let id = table
        .insert(attachment)
        .expect("insert should succeed with room in the table");
    assert!(
        table.get(id).unwrap().as_channel().is_some(),
        "installed handle's resource should be a channel"
    );
}

/// Resource types outside the whitelist (initially: SharedBuffer and
/// ChannelEndpoint) are rejected — checked here via a DirectoryResource,
/// which implements neither `as_shared_buffer` nor `as_channel`.
fn non_whitelisted_resource_is_rejected_by_whitelist() {
    let dir: Arc<dyn Resource> = Arc::new(DirectoryResource::new(Vec::new()));
    assert!(
        !is_transferable(&dir),
        "a directory resource must not be whitelisted for channel transfer"
    );
}

/// A plain message (no attachment) reports `None`, not a spurious handle.
fn message_without_attachment_reports_none() {
    let (a, b) = ChannelEndpoint::create_pair();
    a.send(b"plain").expect("send should succeed");

    let mut buf = [0u8; 64];
    let (len, attachment) = b
        .recv_with_attachment(&mut buf)
        .expect("recv should succeed");
    assert_eq!(&buf[..len], b"plain");
    assert!(
        attachment.is_none(),
        "a message sent via plain send() should report no attachment"
    );
}

/// If the receiver's handle table is full, a message carrying an attachment
/// must stay queued rather than being popped and its attachment lost. This
/// mirrors the guard in `syscall/channel.rs::handle_recv`: check
/// `peek_has_attachment()` / handle-table capacity *before* calling
/// `recv_with_attachment`, which pops unconditionally on success.
fn recv_with_full_handle_table_leaves_message_queued() {
    let (a1, b1) = ChannelEndpoint::create_pair();
    let (_a2, b2) = ChannelEndpoint::create_pair();

    let b2_resource: Arc<dyn Resource> = Arc::new(b2);
    a1.send_with_attachment(b"transfer me", Some(b2_resource))
        .expect("send should succeed");

    // Fill a handle table to the per-process limit, mirroring
    // handle_table_limit_enforced in tests/resource.rs.
    let mut table = HandleTable::new();
    for _ in 0..MAX_HANDLES_PER_PROCESS {
        let filler: Arc<dyn Resource> = Arc::new(DirectoryResource::new(Vec::new()));
        table
            .insert(filler)
            .expect("insert should succeed within the limit");
    }
    assert!(table.is_full(), "table should be at the per-process limit");

    // The message at the front of the queue carries an attachment, and the
    // table has no room — the recv handler's guard would refuse to pop it
    // here. Simulate exactly that: don't call recv_with_attachment.
    assert_eq!(
        b1.peek_has_attachment(),
        Some(true),
        "queued message should be reported as carrying an attachment"
    );

    // Confirm the message really is still queued (not lost) by receiving it
    // now that we're not constrained by a full table.
    let mut buf = [0u8; 64];
    let (len, attachment) = b1
        .recv_with_attachment(&mut buf)
        .expect("message should still be queued and receivable");
    assert_eq!(&buf[..len], b"transfer me");
    assert!(attachment.is_some());
}

/// Sending a channel endpoint through itself is allowed — it's just an Arc
/// clone, so there's no aliasing or reentrancy hazard with the channel's
/// internal lock.
fn endpoint_can_be_sent_through_itself() {
    let (a, b) = ChannelEndpoint::create_pair();
    let a_resource: Arc<dyn Resource> = Arc::new(a);
    let a_channel = a_resource.as_channel().expect("should be a channel");

    a_channel
        .send_with_attachment(b"self", Some(a_resource.clone()))
        .expect("a channel should be able to carry a handle to itself");

    let mut buf = [0u8; 16];
    let (len, attachment) = b
        .recv_with_attachment(&mut buf)
        .expect("recv should succeed");
    assert_eq!(&buf[..len], b"self");
    assert!(attachment.is_some());
}
