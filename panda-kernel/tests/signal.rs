//! Tests for signal encoding/decoding round-trip.

#![no_std]
#![no_main]

use panda_abi::{
    ProcessMessageType, Signal, SignalMessage, SIGNAL_MESSAGE_SIZE, encode_signal_message,
};

panda_kernel::test_harness!(
    signal_enum_from_u32_valid,
    signal_enum_from_u32_invalid,
    encode_signal_message_terminate,
    encode_signal_message_kill,
    encode_signal_message_buffer_too_small,
    decode_signal_message_terminate,
    decode_signal_message_kill,
    decode_signal_message_round_trip_terminate,
    decode_signal_message_round_trip_kill,
    decode_signal_message_not_signal,
    decode_signal_message_truncated,
    decode_signal_message_invalid_signal,
    is_signal_message_check,
);

/// Test Signal::from_u32 with valid values.
fn signal_enum_from_u32_valid() {
    assert_eq!(Signal::from_u32(0), Some(Signal::Terminate));
    assert_eq!(Signal::from_u32(1), Some(Signal::Kill));
}

/// Test Signal::from_u32 with invalid values.
fn signal_enum_from_u32_invalid() {
    assert_eq!(Signal::from_u32(2), None);
    assert_eq!(Signal::from_u32(100), None);
    assert_eq!(Signal::from_u32(u32::MAX), None);
}

/// Test encoding a Terminate signal message.
fn encode_signal_message_terminate() {
    let mut buf = [0u8; SIGNAL_MESSAGE_SIZE];
    let len = encode_signal_message(Signal::Terminate, &mut buf);

    assert_eq!(len, Some(SIGNAL_MESSAGE_SIZE));

    // Verify header fields (little-endian)
    // id (u64) = 0
    assert_eq!(&buf[0..8], &[0, 0, 0, 0, 0, 0, 0, 0]);
    // msg_type (u32) = ProcessMessageType::Signal = 1
    assert_eq!(&buf[8..12], &[1, 0, 0, 0]);
    // _reserved (u32) = 0
    assert_eq!(&buf[12..16], &[0, 0, 0, 0]);
    // signal (u32) = Signal::Terminate = 0
    assert_eq!(&buf[16..20], &[0, 0, 0, 0]);
    // _pad (u32) = 0
    assert_eq!(&buf[20..24], &[0, 0, 0, 0]);
}

/// Test encoding a Kill signal message.
fn encode_signal_message_kill() {
    let mut buf = [0u8; SIGNAL_MESSAGE_SIZE];
    let len = encode_signal_message(Signal::Kill, &mut buf);

    assert_eq!(len, Some(SIGNAL_MESSAGE_SIZE));

    // signal (u32) = Signal::Kill = 1 at offset 16
    assert_eq!(&buf[16..20], &[1, 0, 0, 0]);
}

/// Test encoding fails with buffer too small.
fn encode_signal_message_buffer_too_small() {
    let mut buf = [0u8; 10]; // Too small
    let len = encode_signal_message(Signal::Terminate, &mut buf);
    assert_eq!(len, None);
}

/// Test decoding a Terminate signal message.
fn decode_signal_message_terminate() {
    let mut buf = [0u8; SIGNAL_MESSAGE_SIZE];
    encode_signal_message(Signal::Terminate, &mut buf).unwrap();

    let msg = SignalMessage::decode(&buf).unwrap();
    assert!(msg.is_some());
    let msg = msg.unwrap();
    assert_eq!(msg.id, 0);
    assert_eq!(msg.signal, Signal::Terminate);
}

/// Test decoding a Kill signal message.
fn decode_signal_message_kill() {
    let mut buf = [0u8; SIGNAL_MESSAGE_SIZE];
    encode_signal_message(Signal::Kill, &mut buf).unwrap();

    let msg = SignalMessage::decode(&buf).unwrap();
    assert!(msg.is_some());
    let msg = msg.unwrap();
    assert_eq!(msg.id, 0);
    assert_eq!(msg.signal, Signal::Kill);
}

/// Test round-trip encode/decode for Terminate.
fn decode_signal_message_round_trip_terminate() {
    let original = Signal::Terminate;
    let mut buf = [0u8; SIGNAL_MESSAGE_SIZE];
    encode_signal_message(original, &mut buf).unwrap();

    let decoded = SignalMessage::decode(&buf).unwrap().unwrap();
    assert_eq!(decoded.signal, original);
}

/// Test round-trip encode/decode for Kill.
fn decode_signal_message_round_trip_kill() {
    let original = Signal::Kill;
    let mut buf = [0u8; SIGNAL_MESSAGE_SIZE];
    encode_signal_message(original, &mut buf).unwrap();

    let decoded = SignalMessage::decode(&buf).unwrap().unwrap();
    assert_eq!(decoded.signal, original);
}

/// Test decoding a non-signal message returns None.
fn decode_signal_message_not_signal() {
    // Encode a message with a different msg_type
    let mut buf = [0u8; SIGNAL_MESSAGE_SIZE];
    // id (u64) = 0
    buf[0..8].copy_from_slice(&0u64.to_le_bytes());
    // msg_type (u32) = ProcessMessageType::GetStatus = 0 (not Signal)
    buf[8..12].copy_from_slice(&(ProcessMessageType::GetStatus as u32).to_le_bytes());
    // _reserved (u32) = 0
    buf[12..16].copy_from_slice(&0u32.to_le_bytes());
    // signal (u32) = 0
    buf[16..20].copy_from_slice(&0u32.to_le_bytes());
    // _pad (u32) = 0
    buf[20..24].copy_from_slice(&0u32.to_le_bytes());

    let msg = SignalMessage::decode(&buf).unwrap();
    assert!(msg.is_none(), "Should return None for non-signal message");
}

/// Test decoding a truncated buffer returns error.
fn decode_signal_message_truncated() {
    let buf = [0u8; 10]; // Too short
    let result = SignalMessage::decode(&buf);
    assert!(result.is_err(), "Should return error for truncated buffer");
}

/// Test decoding a message with invalid signal value returns error.
fn decode_signal_message_invalid_signal() {
    let mut buf = [0u8; SIGNAL_MESSAGE_SIZE];
    // id (u64) = 0
    buf[0..8].copy_from_slice(&0u64.to_le_bytes());
    // msg_type (u32) = ProcessMessageType::Signal = 1
    buf[8..12].copy_from_slice(&(ProcessMessageType::Signal as u32).to_le_bytes());
    // _reserved (u32) = 0
    buf[12..16].copy_from_slice(&0u32.to_le_bytes());
    // signal (u32) = 99 (invalid)
    buf[16..20].copy_from_slice(&99u32.to_le_bytes());
    // _pad (u32) = 0
    buf[20..24].copy_from_slice(&0u32.to_le_bytes());

    let result = SignalMessage::decode(&buf);
    assert!(result.is_err(), "Should return error for invalid signal value");
}

/// Test is_signal_message helper.
fn is_signal_message_check() {
    // Valid signal message
    let mut buf = [0u8; SIGNAL_MESSAGE_SIZE];
    encode_signal_message(Signal::Terminate, &mut buf).unwrap();
    assert!(SignalMessage::is_signal_message(&buf));

    // Non-signal message
    buf[8..12].copy_from_slice(&(ProcessMessageType::GetStatus as u32).to_le_bytes());
    assert!(!SignalMessage::is_signal_message(&buf));

    // Buffer too short
    let short_buf = [0u8; 10];
    assert!(!SignalMessage::is_signal_message(&short_buf));
}
