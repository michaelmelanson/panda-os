# Terminal Command Execution Plan

## Goal

Make the terminal execute commands. Typing `hello` at the prompt should spawn `/initrd/hello`, wait for it to exit, then return to the prompt. Arguments are passed via a startup message over an IPC channel.

## Current State

**What works:**
- Terminal renders text, accepts keyboard input, maintains a line buffer
- `environment::spawn("file:/path")` spawns a child process and returns a handle
- `process::wait(handle)` blocks until child exits and returns exit code
- Child processes run correctly (spawn_test proves this)

**What's missing:**
- Terminal's `handle_enter()` just logs the buffer and clears it - no command execution
- No argument passing to child processes
- No IPC mechanism (channel.rs has stubs only)
- No event multiplexing (can't handle keyboard while waiting for child)
- Child stdout goes to kernel log, not terminal window

## Design Overview

Two new primitives work together:

### Channels (data transfer)
- Byte streams between two endpoints
- `send(handle, data)` / `recv(handle, buf)` 
- Spawn creates a channel between parent and child

### Mailboxes (event multiplexing)  
- Aggregate events from multiple handles
- `wait(mailbox)` blocks until any handle has events
- Every process has a default mailbox; handles auto-attach to it

### Argument Passing via Startup Message

Instead of POSIX argc/argv on stack:

1. **Spawn creates a channel** between parent and child
2. **Parent sends startup message** containing args (and later: env, cwd, inherited handles)
3. **Child waits on mailbox** → parent channel becomes readable → recv startup message
4. **libpanda's `main!` macro** handles this transparently

This design:
- Unifies process startup with general IPC
- Makes argument passing extensible (add env vars later without ABI changes)
- Uses the same primitives as everything else (no special cases)
- Enables future pipe support using channels
- Allows a POSIX compatibility layer to be built on top

### Event-Driven Terminal

The mailbox enables proper event handling:

```
Terminal's default mailbox
    │
    ├── keyboard handle (READABLE events)
    │
    └── child handle (READABLE, CLOSED events)
            │
            └── when CLOSED → child exited, show prompt
```

Terminal can handle Ctrl+C during command execution because keyboard events arrive alongside child events.

## Implementation Phases

### Phase 1: Mailbox + Channel Infrastructure

Implement both primitives together since they're interdependent.

#### Mailbox

A mailbox aggregates events from attached handles. Processes can create multiple mailboxes for separate event loops.

**New syscalls:**
```rust
// panda-abi/src/lib.rs

// Well-known handles
pub const HANDLE_SELF: u32 = 0;
pub const HANDLE_ENVIRONMENT: u32 = 1;
pub const HANDLE_MAILBOX: u32 = 2;      // Process's default mailbox
pub const HANDLE_PARENT: u32 = 3;       // Channel to parent (if spawned)

// Mailbox operations (0x7_0000 - 0x7_0FFF)
pub const OP_MAILBOX_CREATE: u32 = 0x7_0000;  // () -> mailbox_handle
pub const OP_MAILBOX_WAIT: u32 = 0x7_0001;    // (mailbox) -> (handle, events) - block until event
pub const OP_MAILBOX_POLL: u32 = 0x7_0002;    // (mailbox) -> (handle, events) or (0, 0) - non-blocking
```

**Resource-specific event flags:**

Each resource type defines its own events:
```rust
// Channel events
pub const CHANNEL_READABLE: u32 = 1 << 0;   // Message available to recv
pub const CHANNEL_WRITABLE: u32 = 1 << 1;   // Space available to send
pub const CHANNEL_CLOSED: u32 = 1 << 2;     // Peer closed

// Keyboard events
pub const KEYBOARD_KEY: u32 = 1 << 0;       // Key event available

// Process events (for spawn handle)
pub const PROCESS_EXITED: u32 = 1 << 0;     // Child process exited
```

**Open/spawn require mailbox + event mask:**

```rust
// Old signatures
pub const OP_ENVIRONMENT_OPEN: u32 = 0x3_0000;   // (path_ptr, path_len, flags) -> handle
pub const OP_ENVIRONMENT_SPAWN: u32 = 0x3_0001;  // (path_ptr, path_len) -> handle

// New signatures
pub const OP_ENVIRONMENT_OPEN: u32 = 0x3_0000;   // (path_ptr, path_len, mailbox, event_mask) -> handle
pub const OP_ENVIRONMENT_SPAWN: u32 = 0x3_0001;  // (path_ptr, path_len, mailbox, event_mask) -> handle
```

This makes attachment explicit and atomic - you can't miss events between open and attach.

**Kernel mailbox implementation:**
```rust
// panda-kernel/src/resource/mailbox.rs

pub struct Mailbox {
    inner: Arc<Spinlock<MailboxInner>>,
}

struct MailboxInner {
    /// Handles attached to this mailbox, with their event masks
    attached: BTreeMap<u32, u32>,  // handle_id -> event_mask
    /// Pending events queue
    pending: VecDeque<(u32, u32)>,  // (handle_id, events)
    /// Waker for process blocked on wait()
    waker: Option<Arc<Waker>>,
}
```

**How resources notify mailboxes:**

Resources hold weak references to their attached mailbox and notify it when events occur:

```rust
// In channel, when data is written:
fn notify_readable(&self) {
    if let Some(mb) = self.mailbox.upgrade() {
        mb.post_event(self.handle_id, CHANNEL_READABLE);
    }
}

// In mailbox:
fn post_event(&self, handle: u32, events: u32) {
    let mut inner = self.inner.lock();
    if let Some(&mask) = inner.attached.get(&handle) {
        let masked = events & mask;
        if masked != 0 {
            inner.pending.push_back((handle, masked));
            if let Some(waker) = inner.waker.take() {
                waker.wake();
            }
        }
    }
}
```

**Resource trait additions:**
```rust
trait Resource {
    // ... existing methods ...
    
    /// What events this resource type can generate
    fn supported_events(&self) -> u32 { 0 }
    
    /// Current pending events (for edge cases / initial state)
    fn poll_events(&self) -> u32 { 0 }
    
    /// Attach to a mailbox with event mask
    fn attach_mailbox(&self, mailbox: Arc<Mailbox>, handle_id: u32, mask: u32);
}
```

#### Channel

Channels are **message-based bounded FIFO queues**. Each `send()` is an atomic message; `recv()` returns one complete message.

**Constraints:**
- Max message size: 1 KB (larger data should use shared memory)
- Bounded queue depth (configurable, e.g., 16 messages)
- `send()` blocks if queue full
- `recv()` blocks if queue empty

**New syscalls:**
```rust
// Channel operations (0x7_1000 - 0x7_1FFF)
pub const OP_CHANNEL_SEND: u32 = 0x7_1000;  // (handle, buf_ptr, buf_len, flags) -> 0 or error
pub const OP_CHANNEL_RECV: u32 = 0x7_1001;  // (handle, buf_ptr, buf_len, flags) -> msg_len or error

// Channel flags
pub const CHANNEL_NONBLOCK: u32 = 1 << 0;   // Don't block, return error if would block
```

**send() semantics:**
- Blocks if queue full (unless `CHANNEL_NONBLOCK` flag set)
- With `CHANNEL_NONBLOCK`: returns error immediately if queue full
- Returns error if message exceeds `MAX_MESSAGE_SIZE`
- Returns error if peer closed

**recv() semantics:**
- Caller provides buffer (should be at least `MAX_MESSAGE_SIZE` bytes)
- Blocks if queue empty (unless `CHANNEL_NONBLOCK` flag set)
- Returns actual message length on success
- Returns error if buffer too small for message
- Returns 0 or error on peer closed

**Kernel channel implementation:**
```rust
// panda-kernel/src/resource/channel.rs

pub struct ChannelEndpoint {
    shared: Arc<Spinlock<ChannelShared>>,
    side: Side,
    handle_id: u32,
    mailbox: Option<Weak<Mailbox>>,
}

enum Side { A, B }

struct ChannelShared {
    /// Messages flowing A→B
    queue_a_to_b: VecDeque<Vec<u8>>,
    /// Messages flowing B→A
    queue_b_to_a: VecDeque<Vec<u8>>,
    /// Is side A closed?
    closed_a: bool,
    /// Is side B closed?
    closed_b: bool,
    /// Mailbox references for notifications
    mailbox_a: Option<(Weak<Mailbox>, u32)>,  // (mailbox, handle_id)
    mailbox_b: Option<(Weak<Mailbox>, u32)>,
    /// Max messages per queue
    capacity: usize,
}

/// Maximum size of a single channel message (1 KB).
/// Larger data should use shared memory / buffer handles.
pub const MAX_MESSAGE_SIZE: usize = 1024;

/// Default queue depth (number of messages).
pub const DEFAULT_QUEUE_CAPACITY: usize = 16;

impl ChannelEndpoint {
    pub fn create_pair() -> (ChannelEndpoint, ChannelEndpoint) {
        let shared = Arc::new(Spinlock::new(ChannelShared::new()));
        (
            ChannelEndpoint { shared: shared.clone(), side: Side::A, handle_id: 0, mailbox: None },
            ChannelEndpoint { shared, side: Side::B, handle_id: 0, mailbox: None },
        )
    }
    
    pub fn send(&self, msg: &[u8]) -> Result<(), ChannelError> {
        if msg.len() > MAX_MESSAGE_SIZE {
            return Err(ChannelError::MessageTooLarge);
        }
        // ... add to queue, notify peer's mailbox with CHANNEL_READABLE
    }
    
    pub fn recv(&self, buf: &mut [u8]) -> Result<usize, ChannelError> {
        // ... pop from queue, notify peer's mailbox with CHANNEL_WRITABLE if was full
        // Return message length, copy into buf
    }
}
```

**Event generation:**
- `CHANNEL_READABLE` - posted to receiver's mailbox when message added to queue
- `CHANNEL_WRITABLE` - posted to sender's mailbox when space becomes available (was full)
- `CHANNEL_CLOSED` - posted when peer closes their endpoint

**Files:**
| File | Change |
|------|--------|
| `panda-abi/src/lib.rs` | Mailbox + channel syscalls, resource-specific event flags, well-known handles |
| `panda-kernel/src/resource/mailbox.rs` | New: Mailbox implementation |
| `panda-kernel/src/resource/channel.rs` | New: ChannelEndpoint with message queues |
| `panda-kernel/src/resource/mod.rs` | Add modules, Resource trait updates |
| `panda-kernel/src/syscall/mailbox.rs` | New: create, wait, poll handlers |
| `panda-kernel/src/syscall/channel.rs` | New: send, recv handlers |
| `panda-kernel/src/syscall/environment.rs` | Update open/spawn to take mailbox + event_mask |
| `panda-kernel/src/syscall/mod.rs` | Route new opcodes |
| `panda-kernel/src/process/mod.rs` | Create default mailbox on process creation |
| `userspace/libpanda/src/mailbox.rs` | New: create, wait, poll wrappers |
| `userspace/libpanda/src/channel.rs` | Update with real send/recv |
| `userspace/libpanda/src/environment.rs` | Update open/spawn signatures |

### Phase 2: Spawn Creates Channel

Modify spawn to create a channel between parent and child.

**Current spawn:**
- Returns process handle (for wait)

**New spawn:**
- Creates bidirectional channel between parent and child
- Child gets channel at `HANDLE_PARENT` (well-known handle 3)
- Parent gets combined handle (channel + process info for wait)
- Both endpoints auto-attach to respective process's default mailbox

**Kernel changes to spawn:**
```rust
// In handle_spawn(), after creating process:

// Create channel pair
let (parent_endpoint, child_endpoint) = ChannelEndpoint::create_pair();

// Give child endpoint to child at HANDLE_PARENT (auto-attaches to child's mailbox)
child_process.handles_mut().insert_at(HANDLE_PARENT, Arc::new(child_endpoint));

// Wrap parent endpoint with process info for wait() support
let spawn_handle = SpawnHandle::new(parent_endpoint, process_info);

// Return to parent (auto-attaches to parent's mailbox)
let handle_id = parent_process.handles_mut().insert(Arc::new(spawn_handle));
```

**SpawnHandle resource:**
```rust
// A handle returned from spawn() - combines channel + process info
struct SpawnHandle {
    channel: ChannelEndpoint,
    process: Arc<ProcessInfo>,
}

impl Resource for SpawnHandle {
    fn as_channel(&self) -> Option<&dyn Channel> { Some(&self.channel) }
    fn as_process(&self) -> Option<&dyn ProcessResource> { Some(&self.process) }
    
    fn event_mask(&self) -> u32 {
        // Report channel events + process exit
        EVENT_READABLE | EVENT_WRITABLE | EVENT_CLOSED
    }
}
```

Parent can:
- `send(child_handle, data)` / `recv(child_handle, buf)` - communicate via channel
- `wait(child_handle)` - wait for child to exit, get exit code
- `mailbox::wait()` - get notified when child has data or exits

**Files:**
| File | Change |
|------|--------|
| `panda-kernel/src/syscall/environment.rs` | Modify handle_spawn to create channel |
| `panda-kernel/src/resource/spawn_handle.rs` | New file: SpawnHandle resource |
| `panda-kernel/src/resource/mod.rs` | Add SpawnHandle |

### Phase 3: Startup Message Protocol

Define the message format and implement helpers.

**Startup message format:**
```rust
// panda-abi/src/lib.rs
#[repr(C)]
pub struct StartupMessageHeader {
    pub version: u16,       // Protocol version (1 for now)
    pub arg_count: u16,     // Number of arguments
    pub env_count: u16,     // Number of environment variables (future)
    pub flags: u16,         // Reserved
    // Followed by: [u16; arg_count] arg_lengths
    // Followed by: packed arg strings (no null terminators)
}
```

**Serialization helpers:**
```rust
// userspace/libpanda/src/startup.rs

/// Serialize arguments into startup message
pub fn encode_startup(args: &[&str]) -> Vec<u8> {
    let mut buf = Vec::new();
    // Write header
    let header = StartupMessageHeader {
        version: 1,
        arg_count: args.len() as u16,
        env_count: 0,
        flags: 0,
    };
    buf.extend_from_slice(bytes_of(&header));
    // Write lengths
    for arg in args {
        buf.extend_from_slice(&(arg.len() as u16).to_le_bytes());
    }
    // Write strings
    for arg in args {
        buf.extend_from_slice(arg.as_bytes());
    }
    buf
}

/// Parse startup message into arguments
pub fn decode_startup(data: &[u8]) -> Option<Vec<String>> {
    let header: StartupMessageHeader = read_from_bytes(data)?;
    // ... parse lengths and strings
}
```

**Files:**
| File | Change |
|------|--------|
| `panda-abi/src/lib.rs` | Add `StartupMessageHeader` |
| `userspace/libpanda/src/startup.rs` | New file: encode/decode helpers |
| `userspace/libpanda/src/lib.rs` | Export startup module |

### Phase 4: Userspace API

Update libpanda with ergonomic, Rust-idiomatic APIs.

#### Mailbox API

```rust
// libpanda/src/mailbox.rs

pub struct Mailbox {
    handle: Handle,
}

impl Mailbox {
    /// Get the default mailbox (HANDLE_MAILBOX)
    pub fn default() -> Self {
        Self { handle: HANDLE_MAILBOX }
    }
    
    /// Create a new mailbox
    pub fn create() -> Result<Self, Error> {
        let handle = syscall::mailbox_create()?;
        Ok(Self { handle })
    }
    
    /// Get raw handle (for passing to open/spawn)
    pub fn handle(&self) -> Handle {
        self.handle
    }
    
    /// Wait for next event (blocking)
    pub fn recv(&self) -> (Handle, Event) {
        let (handle, raw_events) = syscall::mailbox_wait(self.handle);
        let event = Event::decode(raw_events);
        (handle, event)
    }
    
    /// Poll for event (non-blocking)
    pub fn try_recv(&self) -> Option<(Handle, Event)> {
        let (handle, raw_events) = syscall::mailbox_poll(self.handle);
        if handle == 0 && raw_events == 0 {
            None
        } else {
            Some((handle, Event::decode(raw_events)))
        }
    }
}
```

#### Event enum

Events are returned one at a time. If multiple flags are set, the mailbox returns them as separate `recv()` calls.

```rust
// libpanda/src/mailbox.rs

pub enum Event {
    // Keyboard events (self-contained, includes key data)
    Key(KeyEvent),
    
    // Channel events (notifications to take action)
    ChannelReadable,
    ChannelWritable,
    ChannelClosed,
    
    // Process events
    ProcessExited,
}

pub struct KeyEvent {
    pub code: u16,
    pub value: KeyValue,
}

pub enum KeyValue {
    Release = 0,
    Press = 1,
    Repeat = 2,
}

impl Event {
    fn decode(raw: u32) -> Self {
        // Decode raw event flags into Event enum
        // For keyboard, raw contains packed KeyEvent data
        // For channel/process, raw is just the flag
    }
}
```

#### Updated open/spawn

```rust
// libpanda/src/environment.rs

/// Open a resource, attaching to mailbox with event mask.
pub fn open(path: &str, mailbox: &Mailbox, event_mask: u32) -> Result<Handle, Error> {
    syscall::open(path, mailbox.handle(), event_mask)
}

/// Spawn a process, attaching to mailbox with event mask.
pub fn spawn(path: &str, mailbox: &Mailbox, event_mask: u32) -> Result<Handle, Error> {
    syscall::spawn(path, mailbox.handle(), event_mask)
}

/// Spawn a process and send startup message with arguments.
pub fn spawn_with_args(path: &str, args: &[&str], mailbox: &Mailbox, event_mask: u32) -> Result<Handle, Error> {
    let handle = spawn(path, mailbox, event_mask)?;
    
    // Send startup message
    let msg = startup::encode(args);
    channel::send(handle, &msg)?;
    
    Ok(handle)
}
```

#### Example usage

```rust
let mailbox = Mailbox::default();

let keyboard = environment::open(
    "keyboard:/pci/input/0", 
    &mailbox, 
    KEYBOARD_KEY
)?;

let child = environment::spawn_with_args(
    "file:/initrd/ls",
    &["ls", "/mnt"],
    &mailbox,
    CHANNEL_READABLE | PROCESS_EXITED
)?;

loop {
    let (handle, event) = mailbox.recv();
    
    match event {
        Event::Key(key) if key.value == KeyValue::Press => {
            if key.code == KEY_ENTER {
                // handle enter
            }
        }
        Event::ChannelReadable if handle == child => {
            let len = channel::recv(child, &mut buf)?;
            // process child output
        }
        Event::ProcessExited if handle == child => {
            let code = process::wait(child);
            break;
        }
        _ => {}
    }
}
```

#### Child side (main! macro)

```rust
// libpanda/src/lib.rs

#[macro_export]
macro_rules! main {
    ($body:expr) => {
        #[no_mangle]
        pub extern "C" fn _start() -> ! {
            $crate::heap::init();
            
            // Wait for startup message from parent
            let args = $crate::startup::receive_args();
            
            let code = { 
                let args = args;
                $body 
            };
            
            $crate::process::exit(code)
        }
    };
}

// libpanda/src/startup.rs
pub fn receive_args() -> Vec<String> {
    let mailbox = Mailbox::default();
    let (handle, event) = mailbox.recv();
    
    if handle != HANDLE_PARENT {
        return vec![];
    }
    
    if let Event::ChannelReadable = event {
        let mut buf = [0u8; 1024];
        if let Ok(len) = channel::recv(HANDLE_PARENT, &mut buf) {
            return decode(&buf[..len]).unwrap_or_default();
        }
    }
    
    vec![]
}
```

**Child process startup flow:**
1. Kernel creates child with default mailbox at `HANDLE_MAILBOX`
2. Kernel attaches parent channel at `HANDLE_PARENT` to child's mailbox with `CHANNEL_READABLE`
3. Child's `main!` calls `mailbox.recv()` → gets `(HANDLE_PARENT, Event::ChannelReadable)`
4. Child calls `channel::recv(HANDLE_PARENT)` → gets startup message with args

#### Channel API

```rust
// libpanda/src/channel.rs

use panda_abi::MAX_MESSAGE_SIZE;

/// Send a message (blocking if queue full).
pub fn send(handle: Handle, msg: &[u8]) -> Result<(), Error> {
    syscall::channel_send(handle, msg, 0)
}

/// Send a message (non-blocking, fails if queue full).
pub fn try_send(handle: Handle, msg: &[u8]) -> Result<(), Error> {
    syscall::channel_send(handle, msg, CHANNEL_NONBLOCK)
}

/// Receive a message (blocking if queue empty).
pub fn recv(handle: Handle, buf: &mut [u8]) -> Result<usize, Error> {
    syscall::channel_recv(handle, buf, 0)
}

/// Receive a message (non-blocking, fails if queue empty).
pub fn try_recv(handle: Handle, buf: &mut [u8]) -> Result<usize, Error> {
    syscall::channel_recv(handle, buf, CHANNEL_NONBLOCK)
}
```

**Files:**
| File | Change |
|------|--------|
| `userspace/libpanda/src/mailbox.rs` | New: `Mailbox`, `Event`, `KeyEvent`, `KeyValue` |
| `userspace/libpanda/src/environment.rs` | Update `open()`, `spawn()`, add `spawn_with_args()` |
| `userspace/libpanda/src/startup.rs` | Add `receive_args()`, `encode()`, `decode()` |
| `userspace/libpanda/src/channel.rs` | Add `send()`, `try_send()`, `recv()`, `try_recv()` |
| `userspace/libpanda/src/lib.rs` | Update `main!` macro, export new modules |

### Phase 5: Terminal Command Execution

Rewrite terminal to use mailbox-based event loop.

**New terminal structure:**
```rust
use libpanda::mailbox::{Mailbox, Event, KeyEvent, KeyValue};
use libpanda::{environment, channel, process};

struct Terminal {
    mailbox: Mailbox,
    keyboard: Handle,
    surface: Handle,
    foreground_child: Option<Handle>,
    line_buffer: String,
    shift_pressed: bool,
    ctrl_pressed: bool,
    // ... rendering state
}

impl Terminal {
    fn new() -> Self {
        let mailbox = Mailbox::default();
        
        let keyboard = environment::open(
            "keyboard:/pci/input/0",
            &mailbox,
            KEYBOARD_KEY
        ).expect("failed to open keyboard");
        
        // Surface doesn't generate events we care about
        // TODO: Consider if surface needs mailbox at all
        let surface = environment::open(
            "surface:/window",
            &mailbox,
            0
        ).expect("failed to open surface");
        
        Terminal {
            mailbox,
            keyboard,
            surface,
            foreground_child: None,
            line_buffer: String::new(),
            shift_pressed: false,
            ctrl_pressed: false,
        }
    }
    
    fn run(&mut self) {
        self.draw_prompt();
        self.flush();
        
        loop {
            let (handle, event) = self.mailbox.recv();
            
            match event {
                Event::Key(key) => {
                    self.handle_key(key);
                }
                Event::ChannelReadable if Some(handle) == self.foreground_child => {
                    // Child sent output (future: display in terminal)
                    let mut buf = [0u8; 1024];
                    if let Ok(len) = channel::recv(handle, &mut buf) {
                        // Display child output
                    }
                }
                Event::ProcessExited if Some(handle) == self.foreground_child => {
                    self.handle_child_exit(handle);
                }
                _ => {}
            }
        }
    }
    
    fn handle_key(&mut self, key: KeyEvent) {
        // Track modifier state
        match key.code {
            KEY_LEFTSHIFT | KEY_RIGHTSHIFT => {
                self.shift_pressed = key.value != KeyValue::Release;
                return;
            }
            KEY_LEFTCTRL | KEY_RIGHTCTRL => {
                self.ctrl_pressed = key.value != KeyValue::Release;
                return;
            }
            _ => {}
        }
        
        // Only handle key presses, not releases
        if key.value == KeyValue::Release {
            return;
        }
        
        match key.code {
            KEY_ENTER => self.execute_command(),
            KEY_BACKSPACE => self.handle_backspace(),
            KEY_C if self.ctrl_pressed => {
                if let Some(child) = self.foreground_child {
                    // Future: process::signal(child, SIGTERM);
                }
            }
            _ => {
                if let Some(ch) = keycode_to_char(key.code, self.shift_pressed) {
                    self.line_buffer.push(ch);
                    self.print_char(ch);
                    self.flush();
                }
            }
        }
    }
    
    fn execute_command(&mut self) {
        let input = core::mem::take(&mut self.line_buffer);
        let input = input.trim();
        
        self.newline();
        
        if input.is_empty() {
            self.draw_prompt();
            self.flush();
            return;
        }
        
        let args: Vec<&str> = input.split_whitespace().collect();
        let cmd = args[0];
        let path = format!("file:/initrd/{}", cmd);
        
        match environment::spawn_with_args(
            &path,
            &args,
            &self.mailbox,
            CHANNEL_READABLE | PROCESS_EXITED
        ) {
            Ok(child) => {
                self.foreground_child = Some(child);
            }
            Err(_) => {
                self.print(cmd);
                self.print(": command not found");
                self.newline();
                self.draw_prompt();
                self.flush();
            }
        }
    }
    
    fn handle_child_exit(&mut self, child: Handle) {
        let exit_code = process::wait(child);
        
        if exit_code != 0 {
            self.print(&format!("[exited: {}]", exit_code));
            self.newline();
        }
        
        self.foreground_child = None;
        self.draw_prompt();
        self.flush();
    }
    
    fn draw_prompt(&mut self) {
        self.print("> ");
    }
}
```

**Key changes from current terminal:**
- Event loop uses `mailbox.recv()` returning `(Handle, Event)` enum
- Pattern matching on `Event` variants for clean dispatch
- Keyboard events include full `KeyEvent` data (code + press/release)
- Resources opened with explicit `&Mailbox` reference
- Child exit detected via `Event::ProcessExited`
- Future: `Event::ChannelReadable` for child output, Ctrl+C via signals

**Files:**
| File | Change |
|------|--------|
| `userspace/terminal/src/main.rs` | Rewrite with mailbox event loop |

### Phase 6: Basic Utilities

Create programs that use arguments.

**hello:**
```rust
libpanda::main! {
    environment::log("Hello, world!");
    0
}
```

**ls:**
```rust
libpanda::main! {
    let path = args.get(1).map(|s| s.as_str()).unwrap_or("/initrd");
    let uri = format!("file:{}", path);
    
    match environment::opendir(&uri) {
        Ok(dir) => {
            while let Some(entry) = file::readdir(dir) {
                environment::log(entry.name());
            }
            file::close(dir);
            0
        }
        Err(_) => {
            environment::log("ls: cannot access directory");
            1
        }
    }
}
```

**cat:**
```rust
libpanda::main! {
    let Some(path) = args.get(1) else {
        environment::log("usage: cat <file>");
        return 1;
    };
    
    let uri = format!("file:{}", path);
    match environment::open(&uri, 0) {
        Ok(fd) => {
            let mut buf = [0u8; 1024];
            loop {
                let n = file::read(fd, &mut buf);
                if n <= 0 { break; }
                // Log in chunks (temporary until stdout works)
                if let Ok(s) = core::str::from_utf8(&buf[..n as usize]) {
                    environment::log(s);
                }
            }
            file::close(fd);
            0
        }
        Err(_) => {
            environment::log("cat: file not found");
            1
        }
    }
}
```

**Files:**
| File | Change |
|------|--------|
| `userspace/hello/Cargo.toml` | New package |
| `userspace/hello/src/main.rs` | Hello world |
| `userspace/ls/Cargo.toml` | New package |
| `userspace/ls/src/main.rs` | List directory |
| `userspace/cat/Cargo.toml` | New package |
| `userspace/cat/src/main.rs` | Print file |
| `Makefile` | Add to USERSPACE_PROGRAMS, copy to initrd |

## Future Work (Not in This Plan)

### Child stdout to terminal
- Option A: Parent passes a buffer handle, child writes there
- Option B: Inherit terminal's output channel as stdout
- Either way, requires more channel work

### Pipes (`ls | grep foo`)
- Parse `|` in terminal
- Create channel between processes
- Connect stdout of left to stdin of right
- Builds naturally on channel infrastructure

### Signals (Ctrl+C)
- Implement `OP_PROCESS_SIGNAL`
- Terminal detects Ctrl+C, sends signal to foreground child

### Environment variables
- Add to startup message (env_count field already reserved)
- Terminal maintains environment, passes to children

## Testing Strategy

### Phase 1 tests (mailbox + channel)

**Kernel test: `mailbox_channel_test`**
```rust
// Test channel send/recv
let (a, b) = ChannelEndpoint::create_pair();
// ... insert into handles, send data, recv data

// Test mailbox wait
// Create channel, attach to mailbox, send from one end
// mailbox::wait() should return with READABLE event
```

**Userspace test: `channel_test`**
```rust
libpanda::main! {
    // Create channel pair (need syscall for this, or test via spawn)
    // For now, test via parent-child:
    // Parent spawns child, sends message, child echoes back
    0
}
```

### Phase 2 test (spawn creates channel)
```rust
// Existing spawn_test should still work (backwards compatible)
let child = spawn("file:/initrd/spawn_child")?;
let code = wait(child);  // wait() still works on spawn handle
assert_eq!(code, 42);

// New test: spawn_channel_test
let child = spawn("file:/initrd/echo_child")?;
channel::send(child, b"ping")?;
let mut buf = [0u8; 4];
channel::recv(child, &mut buf)?;
assert_eq!(&buf, b"pong");
let code = wait(child);
assert_eq!(code, 0);

// echo_child:
libpanda::main! {
    let mut buf = [0u8; 4];
    channel::recv(HANDLE_PARENT, &mut buf)?;
    channel::send(HANDLE_PARENT, b"pong")?;
    0
}
```

### Phase 3-4 test (startup message)
```rust
// New test: args_test
let child = spawn_with_args("file:/initrd/args_child", &["arg1", "arg2"])?;
let code = wait(child);
assert_eq!(code, 0);

// args_child:
libpanda::main! {
    // args[0] is program name, args[1..] are arguments
    if args.len() == 3 && args[1] == "arg1" && args[2] == "arg2" {
        0
    } else {
        1
    }
}
```

### Phase 5-6 test (terminal + utilities)
Manual test:
1. Boot, terminal shows `> ` prompt
2. Type `hello`, see "Hello, world!" in kernel log
3. Type `ls`, see initrd contents in kernel log
4. Type `ls /mnt`, see ext2 contents
5. Type `cat /mnt/hello.txt`, see file contents
6. Type `nonexistent`, see "command not found"
7. Run a long-running command, press keys while it runs (should queue in mailbox)
8. Future: Ctrl+C kills foreground process

## Summary of All File Changes

| Phase | File | Change |
|-------|------|--------|
| 1 | `panda-abi/src/lib.rs` | Mailbox + channel syscalls, resource-specific event flags, well-known handles, updated open/spawn signatures |
| 1 | `panda-kernel/src/resource/mailbox.rs` | New: Mailbox with event queuing and waker support |
| 1 | `panda-kernel/src/resource/channel.rs` | New: ChannelEndpoint with message queues (1KB max, 16 depth) |
| 1 | `panda-kernel/src/resource/mod.rs` | Add modules, Resource trait with `supported_events()`, `poll_events()`, `attach_mailbox()` |
| 1 | `panda-kernel/src/syscall/mailbox.rs` | New: create, wait, poll handlers |
| 1 | `panda-kernel/src/syscall/channel.rs` | New: send, recv handlers |
| 1 | `panda-kernel/src/syscall/environment.rs` | Update open/spawn to take mailbox + event_mask |
| 1 | `panda-kernel/src/syscall/mod.rs` | Route new opcodes |
| 1 | `panda-kernel/src/process/mod.rs` | Create default mailbox at HANDLE_MAILBOX on process creation |
| 1 | `userspace/libpanda/src/mailbox.rs` | New: create, wait, poll wrappers |
| 1 | `userspace/libpanda/src/channel.rs` | Real send/recv implementations |
| 1 | `userspace/libpanda/src/environment.rs` | Update open/spawn to take mailbox + event_mask |
| 2 | `panda-kernel/src/syscall/environment.rs` | Spawn creates channel pair, child gets HANDLE_PARENT |
| 2 | `panda-kernel/src/resource/spawn_handle.rs` | New: SpawnHandle combining channel + process info |
| 2 | `panda-kernel/src/resource/mod.rs` | Add SpawnHandle |
| 3 | `panda-abi/src/lib.rs` | `StartupMessageHeader` struct |
| 3 | `userspace/libpanda/src/startup.rs` | New: encode/decode helpers |
| 4 | `userspace/libpanda/src/environment.rs` | Add `spawn_with_args()` |
| 4 | `userspace/libpanda/src/startup.rs` | Add `receive_args()` using mailbox::wait |
| 4 | `userspace/libpanda/src/lib.rs` | Update `main!` macro to wait for and parse startup message |
| 5 | `userspace/terminal/src/main.rs` | Rewrite with mailbox event loop |
| 6 | `userspace/hello/*` | New program |
| 6 | `userspace/ls/*` | New program |
| 6 | `userspace/cat/*` | New program |
| 6 | `Makefile` | Build new programs, add to initrd |

## New Tests

| Phase | Test | Description |
|-------|------|-------------|
| 1 | `panda-kernel/tests/mailbox_channel.rs` | Kernel-level mailbox and channel tests |
| 1-2 | `userspace/tests/channel_test/*` | Userspace channel via parent-child |
| 2 | `userspace/tests/echo_child/*` | Child that echoes messages back |
| 3-4 | `userspace/tests/args_test/*` | Test argument passing |
| 3-4 | `userspace/tests/args_child/*` | Child that validates received args |
