#![no_std]
#![no_main]

use libpanda::{environment, file, ipc::channel};

libpanda::main! {
    environment::log("Handle limit test: starting");

    // Create channels in a loop until we hit the per-process handle limit.
    // Each create_pair() call creates 2 handles, so we track them for cleanup.
    let mut handles: [u64; 8192] = [0; 8192];
    let mut count: usize = 0;
    let mut hit_limit = false;

    loop {
        let mut pair: [u64; 2] = [0, 0];
        let result = libpanda::sys::channel::create_raw(&mut pair);
        if result < 0 {
            hit_limit = true;
            break;
        }
        if count + 2 > handles.len() {
            // Safety net — should not reach here before the kernel limit
            break;
        }
        handles[count] = pair[0];
        handles[count + 1] = pair[1];
        count += 2;
    }

    if !hit_limit {
        environment::log("FAIL: never hit handle limit");
        return 1;
    }

    environment::log("Handle limit test: hit limit as expected");

    // Close one handle to free a slot
    if count >= 2 {
        file::close(handles[0].into());

        // Now creating a channel should succeed again (only needs 1 free slot
        // for the first endpoint, but we freed one so there is room)
        // Actually create_pair needs 2 slots. Close another.
        file::close(handles[1].into());

        let mut pair: [u64; 2] = [0, 0];
        let result = libpanda::sys::channel::create_raw(&mut pair);
        if result < 0 {
            environment::log("FAIL: could not create channel after closing handles");
            return 1;
        }
        environment::log("Handle limit test: created channel after freeing slots");
    }

    environment::log("PASS");
    0
}
