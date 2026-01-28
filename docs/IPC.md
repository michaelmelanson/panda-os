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
