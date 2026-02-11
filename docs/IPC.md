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

**Signal events:**
```rust
EVENT_SIGNAL_RECEIVED   // Signal message available
```

## Signals

Signals provide a mechanism for process termination and notification.

### Signal types

| Signal | Value | Behaviour |
|--------|-------|-----------|
| `Signal::Stop` | 0 | Graceful termination request. Delivered as a message on `HANDLE_PARENT`. |
| `Signal::StopImmediately` | 1 | Forced termination. Kernel immediately tears down the process. |

### SIGKILL semantics

When a process receives `Signal::StopImmediately`:
1. Kernel immediately removes it from the scheduler
2. All handles are closed (channels notify peers)
3. Memory is reclaimed
4. Exit code is set to -9
5. Waiters are woken with the exit code

### SIGTERM semantics

When a process receives `Signal::Stop`:
1. A `ProcessSignalRequest` message is sent via the parent-child channel
2. `EVENT_SIGNAL_RECEIVED` and `EVENT_CHANNEL_READABLE` are posted to the child's mailbox
3. The child receives the message on `HANDLE_PARENT`
4. The message uses `MessageHeader` with `msg_type = ProcessMessageType::Signal`
5. The process can catch and handle it gracefully
6. If the channel is full, the syscall returns `WouldBlock`

### Signal message format

Signals use the existing message infrastructure with safe encoding/decoding:

```rust
// MessageHeader (16 bytes)
// - id: u64 = 0 (unsolicited event)
// - msg_type: u32 = ProcessMessageType::Signal
// - _reserved: u32 = 0
// Signal payload (8 bytes)
// - signal: u32 = Signal value
// - _pad: u32 = 0

// Total: 24 bytes (SIGNAL_MESSAGE_SIZE)
```

### API

```rust
use libpanda::process::{Child, Signal};

// Spawn a child
let mut child = Child::spawn("file:/initrd/program")?;

// Send SIGTERM (graceful termination)
child.signal(Signal::Stop)?;

// Send SIGKILL (forced termination)
child.kill()?;  // Shorthand for signal(Signal::StopImmediately)

// Wait for exit
let status = child.wait()?;
```

### Handling signals (child side)

```rust
use panda_abi::{EventFlags, SignalMessage, Signal, SIGNAL_MESSAGE_SIZE, WellKnownHandle};

libpanda::main! {
    let mailbox = Mailbox::default();

    loop {
        let (handle, events) = mailbox.wait();
        let events = EventFlags(events);

        if handle == WellKnownHandle::PARENT && events.is_signal_received() {
            let mut buf = [0u8; SIGNAL_MESSAGE_SIZE];
            if let Ok(len) = channel::try_recv(HANDLE_PARENT, &mut buf) {
                if let Ok(Some(msg)) = SignalMessage::decode(&buf[..len]) {
                    match msg.signal {
                        Signal::Stop => {
                            // Handle graceful shutdown
                            return 0;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}
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
