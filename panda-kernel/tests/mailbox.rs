//! Tests for mailbox event queue bounding and coalescing.

#![no_std]
#![no_main]

extern crate alloc;

use panda_abi::{EventFlags, HandleType};
use panda_kernel::resource::Mailbox;

/// Convenience constant: mask that accepts all event types (bits 0-7).
const ALL_EVENTS_MASK: u32 = 0xFF;

panda_kernel::test_harness!(
    post_event_basic,
    post_event_coalesces_same_handle,
    post_event_bounded_by_max,
    post_event_drops_oldest_when_full,
    post_event_coalesces_before_dropping,
    mailbox_ref_bounded,
    detach_removes_pending_events,
    post_event_after_detach_is_dropped,
    detach_unattached_handle_is_safe,
    post_event_filtered_by_mask,
    post_event_partial_mask_delivers_full_event,
    poll_empty_mailbox_returns_none,
    mailbox_ref_post_after_drop,
    mailbox_ref_delivers_full_event,
);

/// Basic post_event and wait round-trip.
fn post_event_basic() {
    let mailbox = Mailbox::new();
    let handle_id = HandleType::Channel.make_handle(1);

    mailbox.attach(handle_id, ALL_EVENTS_MASK);
    mailbox.post_event(handle_id, EventFlags::CHANNEL_READABLE.0);

    let event = mailbox.wait();
    assert!(event.is_some(), "Should have a pending event");
    let (h, flags) = event.unwrap();
    assert_eq!(h, handle_id);
    assert_eq!(flags, EventFlags::CHANNEL_READABLE.0);

    // Queue should now be empty.
    assert!(!mailbox.has_pending());
}

/// Posting multiple events for the same handle coalesces via OR.
fn post_event_coalesces_same_handle() {
    let mailbox = Mailbox::new();
    let handle_id = HandleType::Channel.make_handle(1);

    mailbox.attach(handle_id, ALL_EVENTS_MASK);

    // Post READABLE, then WRITABLE — should coalesce into one entry.
    mailbox.post_event(handle_id, EventFlags::CHANNEL_READABLE.0);
    mailbox.post_event(handle_id, EventFlags::CHANNEL_WRITABLE.0);

    let event = mailbox.wait();
    assert!(event.is_some());
    let (h, flags) = event.unwrap();
    assert_eq!(h, handle_id);
    let expected = EventFlags::CHANNEL_READABLE.or(EventFlags::CHANNEL_WRITABLE).0;
    assert_eq!(flags, expected, "Flags should be ORed together");

    // No second entry — it was coalesced.
    assert!(!mailbox.has_pending(), "Should have coalesced into one entry");
}

/// Queue does not grow past MAX_MAILBOX_EVENTS.
fn post_event_bounded_by_max() {
    let mailbox = Mailbox::new();
    let limit = panda_abi::MAX_MAILBOX_EVENTS;

    // Attach many distinct handles so coalescing does not kick in.
    for i in 0..limit + 100 {
        let handle_id = HandleType::Channel.make_handle(i as u64);
        mailbox.attach(handle_id, ALL_EVENTS_MASK);
    }

    // Post one event per handle — fill beyond limit.
    for i in 0..limit + 100 {
        let handle_id = HandleType::Channel.make_handle(i as u64);
        mailbox.post_event(handle_id, EventFlags::CHANNEL_READABLE.0);
    }

    // Drain and count.
    let mut count = 0;
    while mailbox.wait().is_some() {
        count += 1;
    }

    assert_eq!(
        count, limit,
        "Queue should be bounded to MAX_MAILBOX_EVENTS"
    );
}

/// When the queue is full and no coalescing is possible, the oldest event
/// is dropped to make room for the new one.
fn post_event_drops_oldest_when_full() {
    let mailbox = Mailbox::new();
    let limit = panda_abi::MAX_MAILBOX_EVENTS;

    // Attach limit + 1 distinct handles.
    for i in 0..=limit {
        let handle_id = HandleType::Channel.make_handle(i as u64);
        mailbox.attach(handle_id, ALL_EVENTS_MASK);
    }

    // Fill to capacity.
    for i in 0..limit {
        let handle_id = HandleType::Channel.make_handle(i as u64);
        mailbox.post_event(handle_id, EventFlags::CHANNEL_READABLE.0);
    }

    // Post one more — should drop handle 0 (oldest).
    let overflow_handle = HandleType::Channel.make_handle(limit as u64);
    mailbox.post_event(overflow_handle, EventFlags::CHANNEL_READABLE.0);

    // The first event we dequeue should be handle 1 (handle 0 was dropped).
    let (first_handle, _) = mailbox.wait().unwrap();
    let expected_first = HandleType::Channel.make_handle(1);
    assert_eq!(
        first_handle, expected_first,
        "Oldest event (handle 0) should have been dropped"
    );
}

/// Coalescing is preferred over dropping when the queue is full.
fn post_event_coalesces_before_dropping() {
    let mailbox = Mailbox::new();
    let limit = panda_abi::MAX_MAILBOX_EVENTS;

    // Fill to capacity with distinct handles, each with CHANNEL_READABLE.
    for i in 0..limit {
        let handle_id = HandleType::Channel.make_handle(i as u64);
        mailbox.attach(handle_id, ALL_EVENTS_MASK);
        mailbox.post_event(handle_id, EventFlags::CHANNEL_READABLE.0);
    }

    // Post another event for handle 0 with a different flag — should coalesce.
    let handle_0 = HandleType::Channel.make_handle(0);
    mailbox.post_event(handle_0, EventFlags::CHANNEL_WRITABLE.0);

    // Queue should still be at exactly limit (coalesced, not grown).
    let mut count = 0;
    let mut found_coalesced = false;
    while let Some((h, flags)) = mailbox.wait() {
        count += 1;
        if h == handle_0 {
            let expected = EventFlags::CHANNEL_READABLE.or(EventFlags::CHANNEL_WRITABLE).0;
            assert_eq!(flags, expected, "Handle 0 flags should be coalesced (READABLE | WRITABLE)");
            found_coalesced = true;
        }
    }
    assert_eq!(count, limit, "Queue should not have grown past limit");
    assert!(found_coalesced, "Should have found the coalesced entry");
}

/// MailboxRef::post_event also respects the bound.
fn mailbox_ref_bounded() {
    use panda_kernel::resource::MailboxRef;

    let mailbox = Mailbox::new();
    let limit = panda_abi::MAX_MAILBOX_EVENTS;

    // Attach many handles.
    for i in 0..limit + 10 {
        let handle_id = HandleType::Channel.make_handle(i as u64);
        mailbox.attach(handle_id, ALL_EVENTS_MASK);
    }

    // Post via MailboxRef for each handle.
    for i in 0..limit + 10 {
        let handle_id = HandleType::Channel.make_handle(i as u64);
        let mbox_ref = MailboxRef::new(&mailbox, handle_id);
        mbox_ref.post_event(EventFlags::CHANNEL_READABLE.0);
    }

    let mut count = 0;
    while mailbox.wait().is_some() {
        count += 1;
    }
    assert_eq!(count, limit, "MailboxRef should also respect the bound");
}

/// Detaching a handle removes its pending events from the queue.
fn detach_removes_pending_events() {
    let mailbox = Mailbox::new();
    let handle_a = HandleType::Channel.make_handle(1);
    let handle_b = HandleType::Channel.make_handle(2);

    mailbox.attach(handle_a, ALL_EVENTS_MASK);
    mailbox.attach(handle_b, ALL_EVENTS_MASK);

    mailbox.post_event(handle_a, EventFlags::CHANNEL_READABLE.0);
    mailbox.post_event(handle_b, EventFlags::CHANNEL_WRITABLE.0);
    assert!(mailbox.has_pending());

    // Detach handle_a — its pending event should be purged.
    mailbox.detach(handle_a);

    // Only handle_b's event should remain.
    let event = mailbox.wait();
    assert!(event.is_some());
    let (h, flags) = event.unwrap();
    assert_eq!(h, handle_b);
    assert_eq!(flags, EventFlags::CHANNEL_WRITABLE.0);
    assert!(!mailbox.has_pending(), "No more events after draining handle_b");
}

/// Events posted to a detached handle are silently dropped.
fn post_event_after_detach_is_dropped() {
    let mailbox = Mailbox::new();
    let handle_id = HandleType::Channel.make_handle(1);

    mailbox.attach(handle_id, ALL_EVENTS_MASK);
    mailbox.detach(handle_id);

    // Post to a handle that's no longer attached — should be dropped.
    mailbox.post_event(handle_id, EventFlags::CHANNEL_READABLE.0);
    assert!(!mailbox.has_pending(), "Event to detached handle should be dropped");
}

/// Detaching a handle that was never attached does not panic.
fn detach_unattached_handle_is_safe() {
    let mailbox = Mailbox::new();
    // Use a valid handle that was never attached to this mailbox.
    let handle_id = HandleType::Channel.make_handle(99);

    // Should not panic.
    mailbox.detach(handle_id);
    assert!(!mailbox.has_pending());
}

/// Events fully rejected by the mask are not queued.
fn post_event_filtered_by_mask() {
    let mailbox = Mailbox::new();
    let handle_id = HandleType::Channel.make_handle(1);

    // Only listen for CHANNEL_READABLE.
    mailbox.attach(handle_id, EventFlags::CHANNEL_READABLE.0);

    // Post an event with only CHANNEL_WRITABLE — no overlap with mask.
    mailbox.post_event(handle_id, EventFlags::CHANNEL_WRITABLE.0);
    assert!(!mailbox.has_pending(), "Event with non-overlapping mask should be dropped");
}

/// Partial mask overlap delivers the full event value (not just the masked bits).
fn post_event_partial_mask_delivers_full_event() {
    let mailbox = Mailbox::new();
    let handle_id = HandleType::Channel.make_handle(1);

    // Listen for CHANNEL_READABLE only.
    mailbox.attach(handle_id, EventFlags::CHANNEL_READABLE.0);

    // Post event with READABLE and WRITABLE — mask overlaps on READABLE,
    // so the event passes the gate. The full value should be delivered.
    let combined = EventFlags::CHANNEL_READABLE.or(EventFlags::CHANNEL_WRITABLE).0;
    mailbox.post_event(handle_id, combined);

    let event = mailbox.wait();
    assert!(event.is_some());
    let (_h, flags) = event.unwrap();
    assert_eq!(flags, combined, "Full event value should be delivered, not just masked bits");
}

/// Polling an empty mailbox returns None without blocking.
fn poll_empty_mailbox_returns_none() {
    let mailbox = Mailbox::new();
    assert!(mailbox.poll().is_none(), "poll() on empty mailbox should return None");
}

/// Posting via MailboxRef after the Mailbox has been dropped does not panic.
fn mailbox_ref_post_after_drop() {
    use panda_kernel::resource::MailboxRef;

    let handle_id = HandleType::Channel.make_handle(1);
    let mbox_ref;
    {
        let mailbox = Mailbox::new();
        mailbox.attach(handle_id, ALL_EVENTS_MASK);
        mbox_ref = MailboxRef::new(&mailbox, handle_id);
        // mailbox is dropped here (Arc refcount for inner goes to 0
        // once the Mailbox and its Arc<Spinlock<MailboxInner>> are dropped).
    }
    // The weak reference inside mbox_ref should fail to upgrade.
    // This must not panic.
    mbox_ref.post_event(EventFlags::CHANNEL_READABLE.0);
}

/// MailboxRef delivers the full event value, not the masked version.
/// This is critical for keyboard events where key codes are encoded in upper bits.
fn mailbox_ref_delivers_full_event() {
    use panda_kernel::resource::MailboxRef;

    let mailbox = Mailbox::new();
    let handle_id = HandleType::Channel.make_handle(1);

    // Listen for KEYBOARD_KEY only.
    mailbox.attach(handle_id, EventFlags::KEYBOARD_KEY.0);

    let mbox_ref = MailboxRef::new(&mailbox, handle_id);

    // Simulate an encoded key event: KEYBOARD_KEY flag + key data in upper bits.
    let encoded_event = panda_abi::encode_key_event(42, 1); // key code 42, press
    mbox_ref.post_event(encoded_event);

    let event = mailbox.wait();
    assert!(event.is_some());
    let (_h, flags) = event.unwrap();
    assert_eq!(
        flags, encoded_event,
        "MailboxRef should deliver the full event including encoded key data"
    );
    // Verify we can decode the key data back.
    assert_eq!(panda_abi::decode_key_code(flags), 42);
    assert_eq!(panda_abi::decode_key_value(flags), 1);
}
