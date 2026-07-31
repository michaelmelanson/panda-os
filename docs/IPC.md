# Inter-Process Communication

Panda OS uses message-passing IPC with two core primitives: channels for data transfer and mailboxes for event multiplexing.

## Channels

Channels are message-based bounded FIFO queues for communication between processes.

### Properties

- **Message-based**: Each `send()` is atomic; `recv()` returns one complete message
- **Max message size**: 4 KB (`MAX_MESSAGE_SIZE`)
- **Queue depth**: 16 messages
- **Blocking**: `send()` blocks if queue full; `recv()` blocks if queue empty
- **Non-blocking variants**: `try_send()` and `try_recv()` return errors instead

### API

```rust
// Create a channel pair
let (a, b) = Channel::create_pair()?;

// Send a message (blocking)
channel::send(handle, &data)?;

// Send a message (non-blocking)
channel::try_send(handle, &data)?;

// Receive a message (blocking)
let len = channel::recv(handle, &mut buf)?;

// Receive a message (non-blocking)
let len = channel::try_recv(handle, &mut buf)?;
```

### Handle transfer

A message can carry one attached handle — a kernel-level analogue of
SCM_RIGHTS over a Unix domain socket. This is how a process hands another
process a resource it can't name by path, such as a shared buffer or a
channel endpoint it created (e.g. sending a client's window buffer to the
compositor).

```rust
use libpanda::ipc::Channel;

// Sender: attach `buffer_handle` to the message. The sender keeps its own
// handle afterwards — this duplicates the resource, it doesn't move it.
channel.send_with_handle(b"here's a buffer", buffer_handle)?;

// Receiver: recv_with_handle reports the transferred handle, if any.
let (len, attached) = channel.recv_with_handle(&mut buf)?;
if let Some(handle) = attached {
    // `handle` is now installed in this process's own handle table.
}
```

Only `SharedBuffer` and `ChannelEndpoint` resources may be attached today —
other resource types are rejected with `InvalidHandle` at send time. See
[SYSCALLS.md](SYSCALLS.md#handle-transfer) for the full ABI, the whitelist
rationale, and edge-case behaviour (handle-table-full, channel-closed, and
self-transfer).

### Spawn Creates Channel

When a process spawns a child:
1. A bidirectional channel is created between parent and child
2. Child receives channel at `HANDLE_PARENT` (well-known handle)
3. Parent receives a combined handle supporting both channel ops and `wait()`

```rust
let child = environment::spawn("file:/initrd/program")?;

// Parent can communicate via channel
channel::send(child, b"hello")?;

// And wait for exit
let exit_code = process::wait(child);
```

## Mailboxes

Mailboxes aggregate events from multiple handles, enabling event-driven programming.

### Properties

- Every process has a default mailbox at `HANDLE_MAILBOX`
- Handles are attached with an event mask specifying which events to receive
- `wait()` blocks until any attached handle has events
- **Queue depth**: bounded to `MAX_MAILBOX_EVENTS` (256) pending entries
- **Coalescing**: when a new event arrives for a handle that already has a pending entry, the event flags are merged (ORed) into the existing entry rather than appending a duplicate — this is safe because mailbox events are level-triggered flags
- **Overflow**: if the queue is full and the event cannot be coalesced, the oldest entry is dropped to make room

### API

```rust
// Get the default mailbox
let mailbox = Mailbox::default();

// Open a resource, attaching to mailbox
let keyboard = environment::open(
    "keyboard:/pci/input/0",
    mailbox.handle(),
    EVENT_KEYBOARD_KEY
)?;

// Spawn a child, attaching to mailbox
let child = environment::spawn(
    "file:/initrd/program",
    mailbox.handle(),
    EVENT_CHANNEL_READABLE | EVENT_PROCESS_EXITED
)?;

// Wait for events
loop {
    let (handle, events) = mailbox.wait();
    
    if handle == keyboard && events.contains(EVENT_KEYBOARD_KEY) {
        // Handle keyboard input
    }
    
    if handle == child && events.contains(EVENT_CHANNEL_READABLE) {
        // Read from child
    }
    
    if handle == child && events.contains(EVENT_PROCESS_EXITED) {
        let code = process::wait(child);
        break;
    }
}
```

### Event Flags

**Channel events:**
```rust
EVENT_CHANNEL_READABLE  // Message available to recv
EVENT_CHANNEL_WRITABLE  // Space available to send
EVENT_CHANNEL_CLOSED    // Peer closed
```

**Keyboard events:**
```rust
EVENT_KEYBOARD_KEY      // Key event available
```

**Process events:**
```rust
EVENT_PROCESS_EXITED    // Child process exited
```

## Well-Known Handles

Every process has these pre-allocated handles. Handle values encode a type tag in the high 8 bits and an ID in the low 24 bits.

| Constant | Type | ID | Description |
|----------|------|-----|-------------|
| `HANDLE_STDIN` | Channel (0x10) | 0 | Data input (pipeline) |
| `HANDLE_STDOUT` | Channel (0x10) | 1 | Data output (pipeline) |
| `HANDLE_STDERR` | Channel (0x10) | 2 | Reserved for error output |
| `HANDLE_PROCESS` | Process (0x11) | 3 | Current process resource |
| `HANDLE_ENVIRONMENT` | Special | 4 | System environment |
| `HANDLE_MAILBOX` | Mailbox (0x20) | 5 | Default mailbox |
| `HANDLE_PARENT` | Channel (0x10) | 6 | Channel to parent process |

## Startup Message Protocol

Arguments are passed from parent to child via a startup message over the channel.

### Message Format

```rust
struct StartupMessageHeader {
    version: u16,       // Protocol version (1)
    arg_count: u16,     // Number of arguments
    env_count: u16,     // Number of environment variables
    flags: u16,         // Reserved
}
// Followed by: [u16; arg_count] arg_lengths
// Followed by: [u16; env_count] key_lengths
// Followed by: [u16; env_count] value_lengths
// Followed by: packed arg strings
// Followed by: packed key strings
// Followed by: packed value strings
```

### Child Startup Flow

1. Kernel creates child with default mailbox at `HANDLE_MAILBOX`
2. Kernel attaches parent channel at `HANDLE_PARENT` to child's mailbox
3. Parent sends startup message with args and environment
4. Child's `main!` macro calls `receive_startup()` to get args and env

### Usage

```rust
// Parent spawns with arguments and environment
let child = Child::builder("file:/initrd/program")
    .args(&["arg1", "arg2"])
    .env("PATH", "/bin")
    .spawn()?;

// Child receives arguments via main! macro
libpanda::main! { |args|
    // args[0] = "program"
    // args[1] = "arg1"
    // args[2] = "arg2"
    0
}

// Or receive both args and environment
libpanda::main! { |args, env|
    let path = env::get("PATH");
    0
}
```

## Event-Driven Pattern

A typical event-driven program:

```rust
libpanda::main! {
    let mailbox = Mailbox::default();
    
    let keyboard = environment::open(
        "keyboard:/pci/input/0",
        mailbox.handle(),
        EVENT_KEYBOARD_KEY
    )?;
    
    loop {
        let (handle, events) = mailbox.wait();
        
        if handle == keyboard {
            let key = keyboard::read(keyboard)?;
            match key.code {
                KEY_Q => break,
                _ => { /* handle key */ }
            }
        }
    }
    
    0
}
```

## Scheme provider protocol (M2.2)

A userspace process becomes a scheme provider with `OP_SCHEME_REGISTER`
(`libpanda::scheme::SchemeProvider::register`), which returns an ordinary
channel handle. The kernel routes `open`/`readdir`/`read`/`write`/`close`
against `<name>:...` to that channel as request/response frames; the
provider serves them with the same `Channel::recv`/`Channel::send` it would
use for any other channel. This section documents the wire format a driver
author needs; see `panda-abi/src/scheme_protocol.rs` for the authoritative
encode/decode implementation and `docs/SYSCALLS.md` "Scheme provider
operations" for the syscall-level contract and v1 scope limitations
(single request in flight per provider, no unregistration, no
`scheme:/<name>` metadata).

### Request frame

```text
byte 0:      kind (u8): 1=Open, 2=Readdir, 3=Read, 4=Write, 5=Close
bytes 1..9:  request_id (u64 LE) — minted by the kernel
bytes 9..:   kind-specific payload
```

| Kind | Payload |
|------|---------|
| `Open` | `path_len: u16`, then `path` bytes (UTF-8) |
| `Readdir` | `path_len: u16`, then `path` bytes (UTF-8) |
| `Read` | `resource_id: u64`, `len: u32` |
| `Write` | `resource_id: u64`, `data_len: u32`, then `data` bytes |
| `Close` | `resource_id: u64` |

`resource_id` is minted by the provider in its `Open` response and is
opaque to the kernel — it only ever echoes it back on later requests for
that same resource.

### Response frame

```text
byte 0:      kind (u8) — same as the request it answers
bytes 1..9:  request_id (u64 LE), echoed verbatim from the request
byte 9:      status (u8): 0=ok, 1=err
bytes 10..:  status- and kind-specific payload
```

On `status=1` (error), the payload is a single `ErrorCode` byte (see
`panda_abi::ErrorCode`'s discriminants; unrecognized bytes collapse to
`IoError`, since a provider is untrusted input). On `status=0` (ok):

| Kind | Ok payload |
|------|------------|
| `Open` | `resource_id: u64` |
| `Readdir` | `count: u16`, then `count` packed entries: `name_len: u8`, `is_dir: u8`, `name` bytes (UTF-8) |
| `Read` | `data_len: u32`, then `data` bytes |
| `Write` | `written: u32` |
| `Close` | *(empty)* |

Every frame — request or response — must fit in one `MAX_MESSAGE_SIZE`
(4 KiB) channel message; there is no fragmentation. `Readdir` responses in
particular are not paginated: a directory listing that doesn't fit in one
frame is a hard error (`MessageTooLarge` from the encoder), not something
this protocol currently handles.

### `request_id` echo requirement

The provider **must** echo the request's `request_id` back verbatim in its
response. v1 only ever has one request in flight per provider (requests
are serialized kernel-side — see docs/SYSCALLS.md), so this isn't needed
for correlating concurrent requests yet. It's still required because a
`Close` triggered by a client dropping its handle is sent fire-and-forget,
without the kernel waiting for the ack — so that ack can arrive interleaved
ahead of a later, unrelated request's real response. The kernel discards
any response whose `request_id` doesn't match the request currently
awaiting one, rather than misdelivering it. Sending a wrong or stale
`request_id` will cause the kernel to hang waiting for the real response
(or, worse, silently swallow it as an orphan) — always copy it from the
request you're answering.

### Provider exit

If the provider process exits (or otherwise closes its endpoint) while a
request is outstanding, or before a later request is sent, that request
fails with `ErrorCode::IoError` rather than hanging. The scheme name stays
registered (pointing at a now-permanently-disconnected provider) — v1 does
not automatically unregister on provider exit.

### Minimal provider example

```rust
use libpanda::scheme::SchemeProvider;
use panda_abi::scheme_protocol::Request;
use panda_abi::{ErrorCode, MAX_MESSAGE_SIZE};

let provider = SchemeProvider::register("echo").unwrap();
let mut buf = [0u8; MAX_MESSAGE_SIZE];
loop {
    let request = match provider.recv(&mut buf) {
        Ok(r) => r,
        Err(_) => break, // kernel's endpoint closed
    };
    match request {
        Request::Open { request_id, path } if path == "/echo" => {
            let _ = provider.reply_open_ok(request_id, 1);
        }
        Request::Open { request_id, .. } => {
            let _ = provider.reply_open_err(request_id, ErrorCode::NotFound);
        }
        // ... Readdir, Read, Write, Close ...
        _ => {}
    }
}
```
