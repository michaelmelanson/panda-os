#![no_std]
#![no_main]

use libpanda::buffer::Buffer;
use libpanda::environment;
use libpanda::ipc;

libpanda::main! {
    environment::log("Size cap test starting");

    // =========================================================================
    // Buffer allocation size caps
    // =========================================================================

    // Test 1: Allocating a zero-size buffer should fail
    if Buffer::alloc(0).is_some() {
        environment::log("FAIL: zero-size buffer alloc should have failed");
        return 1;
    }
    environment::log("PASS: zero-size buffer alloc rejected");

    // Test 2: Allocating a valid buffer should succeed
    let max_buf_size = panda_abi::MAX_BUFFER_SIZE;
    let Some(buf) = Buffer::alloc(4096) else {
        environment::log("FAIL: valid buffer alloc should succeed");
        return 1;
    };
    drop(buf);
    environment::log("PASS: valid buffer alloc succeeded");

    // Test 3: Allocating a buffer exceeding MAX_BUFFER_SIZE should fail
    if Buffer::alloc(max_buf_size + 1).is_some() {
        environment::log("FAIL: oversized buffer alloc should have failed");
        return 1;
    }
    environment::log("PASS: oversized buffer alloc rejected");

    // Test 4: Allocating a buffer with a very large size should fail
    if Buffer::alloc(usize::MAX).is_some() {
        environment::log("FAIL: usize::MAX buffer alloc should have failed");
        return 1;
    }
    environment::log("PASS: usize::MAX buffer alloc rejected");

    // =========================================================================
    // Buffer resize size caps
    // =========================================================================

    // Test 5: Resize to valid size should succeed
    let Some(mut buf) = Buffer::alloc(4096) else {
        environment::log("FAIL: could not allocate buffer for resize test");
        return 1;
    };
    if buf.resize(8192).is_none() {
        environment::log("FAIL: resize to 8192 should succeed");
        return 1;
    }
    environment::log("PASS: buffer resize to valid size succeeded");

    // Test 6: Resize to zero should fail
    if buf.resize(0).is_some() {
        environment::log("FAIL: resize to 0 should have failed");
        return 1;
    }
    environment::log("PASS: buffer resize to zero rejected");

    // Test 7: Resize exceeding MAX_BUFFER_SIZE should fail
    if buf.resize(max_buf_size + 1).is_some() {
        environment::log("FAIL: resize beyond MAX_BUFFER_SIZE should have failed");
        return 1;
    }
    environment::log("PASS: oversized buffer resize rejected");
    drop(buf);

    // =========================================================================
    // Channel message size caps
    // =========================================================================

    // Test 8: Sending a message within MAX_MESSAGE_SIZE should succeed
    let Ok((endpoint_a, endpoint_b)) = ipc::channel::create_pair() else {
        environment::log("FAIL: could not create channel pair");
        return 1;
    };
    let chan_a = ipc::Channel::from_typed(endpoint_a);
    let chan_b = ipc::Channel::from_typed(endpoint_b);

    let small_msg = [0xABu8; 64];
    if chan_a.send(&small_msg).is_err() {
        environment::log("FAIL: small channel send should succeed");
        return 1;
    }
    // Drain the message
    let mut recv_buf = [0u8; 64];
    if chan_b.recv(&mut recv_buf).is_err() {
        environment::log("FAIL: channel recv failed");
        return 1;
    }
    environment::log("PASS: small channel message sent and received");

    // Test 9: Sending exactly MAX_MESSAGE_SIZE should succeed
    let max_msg = [0xCDu8; panda_abi::MAX_MESSAGE_SIZE];
    if chan_a.send(&max_msg).is_err() {
        environment::log("FAIL: MAX_MESSAGE_SIZE send should succeed");
        return 1;
    }
    let mut big_recv_buf = [0u8; panda_abi::MAX_MESSAGE_SIZE];
    if chan_b.recv(&mut big_recv_buf).is_err() {
        environment::log("FAIL: MAX_MESSAGE_SIZE recv failed");
        return 1;
    }
    environment::log("PASS: MAX_MESSAGE_SIZE channel message sent and received");

    // Test 10: Sending a message exceeding MAX_MESSAGE_SIZE should fail
    // We cannot create a stack array larger than MAX_MESSAGE_SIZE easily in no_std,
    // so we use the heap via libpanda's allocator.
    let oversized_msg = libpanda::vec![0xEFu8; panda_abi::MAX_MESSAGE_SIZE + 1];
    if chan_a.send(&oversized_msg).is_ok() {
        environment::log("FAIL: oversized channel send should have failed");
        return 1;
    }
    environment::log("PASS: oversized channel message rejected");

    drop(chan_a);
    drop(chan_b);

    environment::log("Size cap test passed");
    0
}
