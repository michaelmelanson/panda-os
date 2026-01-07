#![no_std]
#![no_main]

use panda_kernel::process::ProcessId;

panda_kernel::test_harness!(
    process_id_unique,
    process_id_monotonic,
    process_id_many_unique
);

fn process_id_unique() {
    let id1 = ProcessId::new();
    let id2 = ProcessId::new();
    assert_ne!(id1, id2);
}

fn process_id_monotonic() {
    let id1 = ProcessId::new();
    let id2 = ProcessId::new();
    let id3 = ProcessId::new();

    assert!(id1 < id2);
    assert!(id2 < id3);
}

fn process_id_many_unique() {
    // Generate many IDs and verify they're all unique
    let mut ids = [ProcessId::new(); 100];
    for i in 1..100 {
        ids[i] = ProcessId::new();
    }

    // Check each pair for uniqueness
    for i in 0..100 {
        for j in (i + 1)..100 {
            assert_ne!(ids[i], ids[j], "IDs at {} and {} should be unique", i, j);
        }
    }
}
