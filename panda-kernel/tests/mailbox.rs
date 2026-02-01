//! Tests for mailbox event queue bounding and coalescing.

#![no_std]
#![no_main]

extern crate alloc;

use panda_kernel::resource::Mailbox;

panda_kernel::test_harness!(
    post_event_basic,
    post_event_coalesces_same_handle,
    post_event_bounded_by_max,
    post_event_drops_oldest_when_full,
    post_event_coalesces_before_dropping,
    mailbox_ref_bounded,
);

/// Basic post_event and wait round-trip.
fn post_event_basic() {
    let mailbox = Mailbox::new();
    let handle_id: u64 = 0x10_0000_0000_0001; // Channel type, id 1

    mailbox.attach(handle_id, 0xFF);
    mailbox.post_event(handle_id, 0x01);

    let event = mailbox.wait();
    assert!(event.is_some(), "Should have a pending event");
    let (h, flags) = event.unwrap();
    assert_eq!(h, handle_id);
    assert_eq!(flags, 0x01);

    // Queue should now be empty.
    assert!(!mailbox.has_pending());
}

/// Posting multiple events for the same handle coalesces via OR.
fn post_event_coalesces_same_handle() {
    let mailbox = Mailbox::new();
    let handle_id: u64 = 0x10_0000_0000_0001;

    mailbox.attach(handle_id, 0xFF);

    // Post READABLE, then WRITABLE — should coalesce into one entry.
    mailbox.post_event(handle_id, 0x01); // CHANNEL_READABLE
    mailbox.post_event(handle_id, 0x02); // CHANNEL_WRITABLE

    let event = mailbox.wait();
    assert!(event.is_some());
    let (h, flags) = event.unwrap();
    assert_eq!(h, handle_id);
    assert_eq!(flags, 0x03, "Flags should be ORed together");

    // No second entry — it was coalesced.
    assert!(!mailbox.has_pending(), "Should have coalesced into one entry");
}

/// Queue does not grow past MAX_MAILBOX_EVENTS.
fn post_event_bounded_by_max() {
    let mailbox = Mailbox::new();
    let limit = panda_abi::MAX_MAILBOX_EVENTS;

    // Attach many distinct handles so coalescing does not kick in.
    for i in 0..limit + 100 {
        let handle_id: u64 = 0x10_0000_0000_0000 | (i as u64);
        mailbox.attach(handle_id, 0xFF);
    }

    // Post one event per handle — fill beyond limit.
    for i in 0..limit + 100 {
        let handle_id: u64 = 0x10_0000_0000_0000 | (i as u64);
        mailbox.post_event(handle_id, 0x01);
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
        let handle_id: u64 = 0x10_0000_0000_0000 | (i as u64);
        mailbox.attach(handle_id, 0xFF);
    }

    // Fill to capacity.
    for i in 0..limit {
        let handle_id: u64 = 0x10_0000_0000_0000 | (i as u64);
        mailbox.post_event(handle_id, 0x01);
    }

    // Post one more — should drop handle 0 (oldest).
    let overflow_handle: u64 = 0x10_0000_0000_0000 | (limit as u64);
    mailbox.post_event(overflow_handle, 0x01);

    // The first event we dequeue should be handle 1 (handle 0 was dropped).
    let (first_handle, _) = mailbox.wait().unwrap();
    let expected_first: u64 = 0x10_0000_0000_0001;
    assert_eq!(
        first_handle, expected_first,
        "Oldest event (handle 0) should have been dropped"
    );
}

/// Coalescing is preferred over dropping when the queue is full.
fn post_event_coalesces_before_dropping() {
    let mailbox = Mailbox::new();
    let limit = panda_abi::MAX_MAILBOX_EVENTS;

    // Fill to capacity with distinct handles, each with flag 0x01.
    for i in 0..limit {
        let handle_id: u64 = 0x10_0000_0000_0000 | (i as u64);
        mailbox.attach(handle_id, 0xFF);
        mailbox.post_event(handle_id, 0x01);
    }

    // Post another event for handle 0 with a different flag — should coalesce.
    let handle_0: u64 = 0x10_0000_0000_0000;
    mailbox.post_event(handle_0, 0x02);

    // Queue should still be at exactly limit (coalesced, not grown).
    let mut count = 0;
    let mut found_coalesced = false;
    while let Some((h, flags)) = mailbox.wait() {
        count += 1;
        if h == handle_0 {
            assert_eq!(flags, 0x03, "Handle 0 flags should be coalesced (0x01 | 0x02)");
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
        let handle_id: u64 = 0x10_0000_0000_0000 | (i as u64);
        mailbox.attach(handle_id, 0xFF);
    }

    // Post via MailboxRef for each handle.
    for i in 0..limit + 10 {
        let handle_id: u64 = 0x10_0000_0000_0000 | (i as u64);
        let mbox_ref = MailboxRef::new(&mailbox, handle_id);
        mbox_ref.post_event(0x01);
    }

    let mut count = 0;
    while mailbox.wait().is_some() {
        count += 1;
    }
    assert_eq!(count, limit, "MailboxRef should also respect the bound");
}
