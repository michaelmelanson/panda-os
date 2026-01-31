# System initialization tool

## Problem

The `init` process (`userspace/init/src/main.rs`) is a hardcoded Rust binary that mounts ext2 at `/mnt` and spawns the terminal. Adding or reordering services requires recompiling. There is no way to declare dependencies between services, restart crashed services, or configure service properties (environment, stdio) without changing code. There is also no mechanism to manage services at runtime — stopping, starting, or adding services after boot.

## Goal

Build a service manager that reads declarative TOML service configurations, computes a plan (a DAG of actions) to bring the system into the desired state, and executes it. The service manager continues running as a supervisor — monitoring services, restarting them according to policy, and accepting runtime commands to start, stop, add, and remove services. Runtime changes go through the same planning pipeline: compute a new plan from the delta between current state and desired state, then execute it.

As part of this effort, establish a **service protocol framework** — a common infrastructure for defining typed IPC protocols between processes. Protocols are the primary interface abstraction in the system: a protocol defines the contract between a service and its clients, identified by a UUID and negotiated via capabilities. This framework is foundational — not just for the service manager, but for any service in the system, including future userspace device drivers.

## Constraints

- **Filesystem is read-only (ext2 has no write support).** Service configs are baked into the ext2 image at build time. Runtime service additions come via IPC commands.
- **Max message size is 4096 bytes.** Channel messages (startup, commands, responses) are bounded by this.
- **Userspace is `no_std` + `alloc`.** Any dependencies must work without `std`.

## Design

### Service protocol framework

#### Protocols as interface contracts

A **protocol** is the fundamental interface abstraction in panda's IPC system. It defines the typed messages exchanged between two endpoints over a channel, identified by a UUID and negotiated via capabilities at connection time.

Today, the kernel exposes device functionality through scheme names: `keyboard:`, `block:`, `console:`. These scheme names implicitly define an interface — opening `keyboard:/pci/input/0` gives you a resource that produces key events. But the interface contract is embedded in the kernel's scheme handler code with no formal definition or type safety.

Protocols make this contract explicit and portable. `Keyboard`, `Block`, `Console` become **protocol definitions** — each with a UUID, typed request/response/event messages, and capability constants. A protocol can be implemented by a kernel-side driver today and a userspace service tomorrow, and clients don't change because they program against the protocol, not the transport.

This means:
- The **protocol UUID** is the real type safety mechanism. When a client connects to any service, the handshake verifies both sides speak the same protocol. The service name is just how you find it.
- **Multiple implementations** of the same protocol are possible. A PS/2 keyboard driver and a virtio keyboard driver both implement the `Keyboard` protocol, registered under different service names (`service:/ps2-keyboard`, `service:/virtio-keyboard`).
- **Discovery by protocol** is supported. A client that wants "any keyboard" can ask the service manager for services implementing a given protocol UUID, rather than hardcoding a service name.
- The **existing kernel schemes** (`keyboard:`, `block:`, etc.) are the current transport for these interfaces. When drivers move to userspace, the same protocol definitions apply — only the transport changes from kernel resource to service channel.

#### Protocol and Service traits

The framework is built on two traits in `panda-abi`:

```rust
/// Defines a wire protocol for a channel connection.
///
/// A protocol specifies the message types exchanged between two endpoints.
/// Both sides of a connection depend on the same Protocol implementation
/// (typically via a shared API crate) for type safety.
///
/// Protocols are the primary interface abstraction: keyboard, block, console,
/// compositor, service-manager are all protocols. The UUID identifies the
/// interface contract, not the implementation.
trait Protocol {
    /// Unique identifier for this protocol's wire format.
    /// Used during handshake to verify both sides speak the same protocol.
    /// A new UUID should be generated when the wire format changes in a
    /// backwards-incompatible way.
    const UUID: [u8; 16];

    /// Request type (initiator -> responder).
    type Request: Encode + Decode;

    /// Response type (responder -> initiator).
    type Response: Encode + Decode;

    /// Event type (responder -> initiator, unsolicited).
    type Event: Encode + Decode;
}

/// A discoverable service registered in the service scheme.
///
/// Extends Protocol with a name used for service:/ discovery.
/// Not all protocols are services — HANDLE_PARENT uses the terminal
/// protocol but is not discoverable via the service scheme.
trait Service: Protocol {
    /// The service name used for `service:/{name}` resolution.
    const NAME: &str;
}
```

The separation between `Protocol` and `Service` is important:

- A **protocol** defines message types and wire format. It's the interface contract.
- A **service** adds discoverability via the `service:` scheme. It's a running process that implements a protocol and is reachable by name.

Not all protocol connections are services. The terminal protocol is used over `HANDLE_PARENT` — a connection inherited from your parent process, not discovered by name. Multiple terminals can coexist because each is its own process with channels to its children. A service like a compositor is discovered by name via `service:/compositor` and is typically a singleton.

#### Message framing

Every message on a protocol channel has a common envelope so the receiver can distinguish message kinds without knowing the specific protocol:

```
+----------+----------+----------+-------------+
| Kind(u8) | Type(u16)| Len(u32) | Payload ... |
+----------+----------+----------+-------------+
```

Where `Kind` is:
- `0x00` — Request (initiator -> responder)
- `0x01` — Response (responder -> initiator)
- `0x02` — Event (responder -> initiator, unsolicited)
- `0x03` — Handshake

The `Type` and `Len` fields are the existing TLV header used by the terminal protocol. The `Kind` byte is prepended, making the framing backwards-distinguishable from the existing terminal protocol (which starts with a `u16` type field, never `0x00`-`0x03` in the high byte).

#### Handshake

Every protocol connection begins with a handshake. The initiator (client or parent) sends a `Hello`, the responder (service or child) replies with `Welcome` or `Rejected`:

```
Initiator -> Responder:  Hello { protocol_uuid: [u8; 16], capabilities: Vec<String> }
Responder -> Initiator:  Welcome { capabilities: Vec<String> }
                         or Rejected { reason: String }
```

The responder checks the UUID first. If it doesn't match the protocol it implements, it sends `Rejected`. Otherwise, it examines the offered capabilities and responds with the subset it supports. Both sides then know the agreed feature set for the lifetime of the connection.

Capabilities are protocol-defined strings. Each protocol's API crate defines its capability constants:

```rust
// in service-keyboard-api
pub mod capability {
    pub const RAW_SCANCODES: &str = "raw-scancodes";
    pub const REPEAT_EVENTS: &str = "repeat-events";
}
```

A protocol with no optional features sends an empty capabilities list — the UUID alone establishes that both sides speak the same language.

#### API crate pattern

Each protocol is published as an API crate containing:
- A zero-sized struct implementing `Protocol` (and `Service` if discoverable)
- The `Request`, `Response`, and `Event` enums with `Encode`/`Decode` implementations
- Capability constants

These crates contain no implementation — just types and encoding. The service binary and client code both depend on the same API crate.

Examples of protocol API crates:

```rust
// service-keyboard-api/src/lib.rs — keyboard interface contract

pub struct Keyboard;

impl Protocol for Keyboard {
    const UUID: [u8; 16] = [/* generated once */];
    type Request = KeyboardRequest;
    type Response = KeyboardResponse;
    type Event = KeyboardEvent;
}

pub enum KeyboardRequest {
    SetRepeatRate { delay_ms: u16, interval_ms: u16 },
}

pub enum KeyboardResponse {
    Ok,
    Error { message: String },
}

pub enum KeyboardEvent {
    Key { code: u16, value: u8 },  // press/release/repeat
}

pub mod capability {
    pub const RAW_SCANCODES: &str = "raw-scancodes";
    pub const REPEAT_EVENTS: &str = "repeat-events";
}
```

```rust
// service-manager-api/src/lib.rs — service manager interface contract

pub struct Manager;

impl Protocol for Manager {
    const UUID: [u8; 16] = [/* generated once */];
    type Request = ManagerRequest;
    type Response = ManagerResponse;
    type Event = ManagerEvent;
}

impl Service for Manager {
    const NAME: &str = "manager";
}

pub enum ManagerRequest {
    Start { name: String },
    Stop { name: String },
    Restart { name: String },
    Status { name: String },
    List,
    ListByProtocol { protocol_uuid: [u8; 16] },
    Add { name: String, config: String },
    Remove { name: String },
}

pub enum ManagerResponse {
    Ok { message: String },
    Error { message: String },
    Status {
        name: String,
        state: String,
        protocol_uuid: Option<[u8; 16]>,
        restart_count: u32,
    },
    List { services: Value },  // Value::Table for pipeline compatibility
}

pub enum ManagerEvent {
    ServiceStateChanged {
        name: String,
        old_state: String,
        new_state: String,
    },
}

pub mod capability {
    pub const RUNTIME_ADD: &str = "runtime-add";
    pub const LOG_STREAMING: &str = "log-streaming";
}
```

#### Client and server wrappers

`libpanda` provides typed wrappers generic over the traits:

```rust
/// A typed channel connection using a specific protocol.
///
/// Wraps a raw channel handle with typed send/receive methods.
/// Used for any protocol connection, whether established by spawn
/// (HANDLE_PARENT) or by the service scheme.
pub struct ProtocolChannel<P: Protocol> {
    handle: Handle,
    _marker: PhantomData<P>,
}

impl<P: Protocol> ProtocolChannel<P> {
    /// Wrap an existing channel handle (e.g., HANDLE_PARENT).
    pub fn from_handle(handle: Handle) -> Self { ... }

    /// Perform the handshake as the initiator (client/parent).
    pub fn handshake_initiate(&self, capabilities: &[&str]) -> Result<Vec<String>> { ... }

    /// Perform the handshake as the responder (service/child).
    pub fn handshake_respond(&self, supported: &[&str]) -> Result<Vec<String>> { ... }

    /// Send a request.
    pub fn send_request(&self, request: &P::Request) -> Result<()> { ... }

    /// Receive and decode the next message (request, response, or event).
    pub fn recv(&self, buf: &mut [u8]) -> Result<Message<P>> { ... }

    /// Send a request and block until the response arrives.
    pub fn call(&self, request: &P::Request) -> Result<P::Response> { ... }

    /// Send a response.
    pub fn send_response(&self, response: &P::Response) -> Result<()> { ... }

    /// Send an unsolicited event.
    pub fn send_event(&self, event: &P::Event) -> Result<()> { ... }

    /// Get the underlying handle (for mailbox attachment).
    pub fn handle(&self) -> Handle { ... }
}

/// A decoded message from a protocol channel.
pub enum Message<P: Protocol> {
    Request(P::Request),
    Response(P::Response),
    Event(P::Event),
}

/// Client-side connection to a discoverable service.
///
/// Connects via the service scheme and performs the handshake automatically.
pub struct ServiceClient<S: Service> {
    channel: ProtocolChannel<S>,
    capabilities: Vec<String>,
}

impl<S: Service> ServiceClient<S> {
    /// Connect to a service by opening service:/{name} and performing the handshake.
    pub fn connect(mailbox: Handle, events: u32, capabilities: &[&str]) -> Result<Self> {
        let handle = environment::open(
            &format!("service:/{}", S::NAME),
            mailbox, events,
        )?;
        let channel = ProtocolChannel::from_handle(handle);
        let negotiated = channel.handshake_initiate(capabilities)?;
        Ok(Self { channel, capabilities: negotiated })
    }

    /// Get the negotiated capabilities.
    pub fn capabilities(&self) -> &[String] { &self.capabilities }

    /// Send a request and wait for the response.
    pub fn call(&self, request: &S::Request) -> Result<S::Response> {
        self.channel.call(request)
    }

    /// Send a request without waiting for a response.
    pub fn send(&self, request: &S::Request) -> Result<()> {
        self.channel.send_request(request)
    }

    /// Get the underlying protocol channel.
    pub fn channel(&self) -> &ProtocolChannel<S> { &self.channel }
}
```

#### Terminal protocol retrofit (future work)

The existing terminal protocol (`terminal::Request`/`terminal::Event`) is conceptually a `Protocol` implementation. Once the framework is in place, it can be retrofitted:

```rust
pub struct TerminalProtocol;

impl Protocol for TerminalProtocol {
    const UUID: [u8; 16] = [/* fixed UUID for terminal protocol */];
    type Request = terminal::Request;
    type Response = terminal::Event;  // QueryResponse, InputResponse
    type Event = terminal::Event;     // Signal, Resize, Key
}
```

The terminal is not a `Service` because it's not discovered by name — it's a parent-child connection inherited via `HANDLE_PARENT`. Multiple terminals coexist because each is its own process with channels to its children.

Retrofitting requires:
1. Adding the `Kind` byte prefix to terminal messages
2. Adding the handshake to the startup sequence (after the existing startup message, or replacing it)
3. Updating `libpanda/src/terminal.rs` to use `ProtocolChannel<TerminalProtocol>`
4. Updating the terminal emulator to respond to the handshake

This is deferred until after the service manager is working. The service manager acts as a minimal terminal protocol peer for its children using the existing unframed `Request`/`Event` messages. The retrofit unifies the patterns but is not a prerequisite.

### Service scheme and discovery

The `service:` scheme is a flat name → channel broker. The path is exactly one segment — the service name:

```
service:/{name}
```

The scheme maps names to channels, nothing more. The service name is an organizational label — the **protocol UUID** is the real interface contract. Two services with different names can implement the same protocol (e.g., `service:/ps2-keyboard` and `service:/virtio-keyboard` both speak the `Keyboard` protocol). A client that knows the service name connects directly; a client that wants "any service implementing this protocol" queries the service manager via `ManagerRequest::ListByProtocol`.

This design keeps the scheme trivial and puts richer semantics (protocol-based discovery, categorization) in the service manager where they're easier to evolve.

**Relationship to existing kernel schemes:**

Today, `keyboard:`, `block:`, `console:` are kernel-side schemes because the drivers are kernel-side. These scheme names implicitly define an interface contract. As drivers move to userspace, the protocol definitions (`Keyboard`, `Block`, `Console`) become the explicit contract, and the transport shifts from kernel resource to service channel. The existing kernel schemes would eventually become unnecessary — clients would connect to `service:/keyboard` instead of opening `keyboard:/pci/input/0`. During migration, both paths can coexist.

### TOML parser

Use the `toml` crate (v0.9+) with `default-features = false, features = ["parse"]` for `no_std` + `alloc` TOML parsing. If it doesn't compile cleanly in panda's userspace (untested), fall back to `toml_edit` with `default-features = false` or a hand-written parser for the TOML subset needed (string values, string arrays, tables).

### Service configuration format

Each service has a directory under `/config/services/` containing a `config.toml`. The service name is the directory name.

```toml
# /config/services/terminal/config.toml
[service]
exec = "file:/mnt/terminal"
```

```toml
# /config/services/networkd/config.toml
[service]
exec = "file:/mnt/networkd"
args = ["--interface", "virtio0"]
stdout = "log"

[service.env]
LOG_LEVEL = "info"

[dependencies]
after = ["block-ready"]

[restart]
policy = "always"
delay_ms = 1000
max_attempts = 10
```

**Schema:**

```toml
[service]
exec = "file:/path"           # Required. Executable URI.
args = ["arg1", "arg2"]       # Optional. Default: []
stdout = "inherit"             # Optional. "inherit", "log", "null", "console". Default: "inherit"
protocol = "keyboard"          # Optional. Protocol name for discovery. Default: none.

[service.env]                  # Optional. Environment variables.
KEY = "value"

[dependencies]
after = ["service-a"]          # Optional. Services that must start first. Default: []

[restart]
policy = "no"                  # Optional. "no", "on-failure", "always". Default: "no"
delay_ms = 1000                # Optional. Delay between restart attempts. Default: 1000
max_attempts = 10              # Optional. Max consecutive restarts before giving up. Default: 10
```

The `protocol` field declares which protocol this service implements. This is metadata used by the service manager for protocol-based discovery queries (`ManagerRequest::ListByProtocol`). The actual protocol verification happens at connection time via the UUID handshake — the config field is for indexing, not enforcement.

### Planning: from desired state to action DAG

The service manager operates on a **desired state** (the set of service configs that should be running) and a **current state** (what's actually running). The planner computes a DAG of actions to transition from current to desired.

**Actions:**

```rust
enum Action {
    Start { service: String },
    Stop { service: String },
    Restart { service: String },  // stop then start
}
```

**Planning steps:**

1. **Validate configs** — check required fields, recognized values, no duplicate names.
2. **Build dependency graph** — for each service, map `after` to service names. Verify all referenced names exist.
3. **Detect cycles** — DFS with coloring (white/gray/black). Report full cycle path (e.g., "cycle: A -> B -> C -> A"). Exclude cyclic services from the plan.
4. **Diff current vs desired state:**
   - Services in desired but not running → `Action::Start` (respecting dependency order)
   - Services running but not in desired → `Action::Stop` (reverse dependency order — stop dependents first)
   - Services with changed config → `Action::Restart`
   - Services already running with matching config → no action
5. **Topological sort actions** — Start actions ordered by dependencies (start dependencies first). Stop actions in reverse order (stop dependents first). Services at the same depth sorted alphabetically for determinism.
6. **Produce plan** — a `Vec<PlanStep>` where each step has an action and a set of prerequisites (indices of steps that must complete first):

```rust
struct Plan {
    steps: Vec<PlanStep>,
    warnings: Vec<String>,
}

struct PlanStep {
    action: Action,
    after: Vec<usize>,  // indices into steps that must complete first
}
```

**Boot is just a special case**: current state is empty, desired state is all configs from `/config/services/`. The planner produces a DAG of Start actions in dependency order.

**Runtime changes** go through the same planner. When a `start`, `stop`, or `add` command arrives, the planner diffs the new desired state against current state and produces a plan. This keeps the logic uniform — there is one code path for all state transitions.

### Service lifecycle states

```
         ┌──────────┐
         │ Waiting   │  (plan step blocked on prerequisites)
         └────┬─────┘
              │ prerequisites complete
              ▼
         ┌──────────┐
         │ Starting  │  (spawn issued)
         └────┬─────┘
              │ spawn succeeds
              ▼
         ┌──────────┐
    ┌───▶│ Running   │◀─────────────────────┐
    │    └────┬─────┘                       │
    │         │ process exits               │ start command
    │         ▼                             │
    │    ┌──────────┐               ┌───────┴──────┐
    │    │ Exited    │               │ Stopped       │  (via stop command)
    │    └────┬─────┘               └──────────────┘
    │         │ restart policy
    │         ▼
    │    ┌──────────┐
    │    │ Restarting│  (timer pending)
    │    └────┬─────┘
    │         │ EVENT_TIMER fires
    └─────────┘
```

States:
- **Waiting**: Plan step blocked on prerequisites. Transitions to Starting when all prerequisite steps complete.
- **Starting**: Spawn issued. Transitions to Running on success, Exited on failure.
- **Running**: Process alive. Handle attached to mailbox with `EVENT_PROCESS_EXITED`.
- **Exited**: Process terminated. Evaluates restart policy. If restarting, creates a timer resource and transitions to Restarting. If `max_attempts` exceeded, stays Exited permanently.
- **Restarting**: Timer pending. When `EVENT_TIMER` fires, transitions to Starting.
- **Stopped**: Stopped via runtime command (SIGTERM → wait → SIGKILL). Does not auto-restart. Can be started again via command.

### Restart backoff

When a service exits and its restart policy applies:

1. Increment consecutive restart counter.
2. If counter exceeds `max_attempts`, log error, stay Exited permanently.
3. Otherwise, create a timer resource for `delay_ms` and attach to mailbox. Enter Restarting.
4. When `EVENT_TIMER` fires, transition to Starting and re-spawn.
5. Counter resets to 0 when a service has been Running for longer than `delay_ms * 2` (tracked by creating a "stability timer" on start — if it fires while still Running, the service is considered stable and the counter resets).

### Stopping a service

When a stop command arrives (or a service needs to be stopped for a restart/removal):

1. Send `Event::Signal(Signal::Terminate)` on the service's parent channel (the spawn handle supports channel operations).
2. Create a timer resource for a grace period (e.g., 3000ms) and attach to mailbox.
3. If `EVENT_PROCESS_EXITED` fires before the timer → clean exit, transition to Stopped.
4. If `EVENT_TIMER` fires first → call `process::kill(handle)` (kernel SIGKILL), wait for exit, transition to Stopped.

### IPC architecture

Following the panda IPC conventions (see [docs/IPC.md](../docs/IPC.md) and [docs/PIPELINES.md](../docs/PIPELINES.md)), the service manager uses the **control plane** (`HANDLE_PARENT` channel) for communication with managed services, and a **service protocol connection** for management commands from tools like `svcctl`.

**Service manager ↔ managed services:**

Each spawned service gets a `HANDLE_PARENT` channel back to the service manager. This is the standard parent-child channel created by `environment::spawn()`. The service manager uses it for:

- Sending the startup message (args + env) per the existing startup protocol
- Receiving structured log output from services that use `Request::Write(Value)` or `Request::Error(Value)` on their parent channel (following the terminal protocol pattern from PIPELINES.md)
- Sending `Event::Signal(Signal)` to request graceful shutdown (same protocol the terminal uses to signal Ctrl+C to children)

This means services don't need special awareness of the service manager — they use the same `HANDLE_PARENT` protocol they'd use with a terminal. The service manager acts as a minimal terminal protocol peer for its children.

**Service manager ↔ management tools (`svcctl`):**

The service manager exposes the `Manager` protocol via the service scheme. `svcctl` connects using `ServiceClient<Manager>`, which opens `service:/manager`, performs the UUID + capabilities handshake, and then exchanges typed `ManagerRequest`/`ManagerResponse` messages.

Using typed requests provides compile-time safety — `svcctl` can't send a malformed command, and the service manager's dispatch is a `match` on `ManagerRequest` variants rather than string key lookups.

The `List` response uses `Value::Table` so that `svcctl list | grep running` works with the structured pipeline system. `svcctl` writes the response `Value` to `HANDLE_PARENT` (or `HANDLE_STDOUT` in a pipeline).

**Service scheme broker:**

All resource schemes are currently kernel-side only. To allow arbitrary processes to connect to the service manager, add a **kernel-side `service:` scheme** that brokers channel connections.

1. At boot, after init creates its mailbox, it registers a channel endpoint with the kernel via a new syscall `OP_SERVICE_REGISTER`. The kernel stores this endpoint in the `service:` scheme handler.
2. When any process opens `service:/manager`, the kernel-side `ServiceScheme` handler creates a new channel pair, sends one endpoint to init (via the registered channel — init receives it as a message containing the new handle), and returns the other endpoint to the caller.
3. Init attaches each incoming connection to its mailbox with `EVENT_CHANNEL_READABLE`.
4. The handshake occurs over the new channel using the protocol framework — init verifies the UUID matches the `Manager` protocol and negotiates capabilities.

Initially only init can register services. The design generalizes to per-process registration (any process calls `OP_SERVICE_REGISTER` with a name and gets a broker channel) for the userspace driver migration, but that's out of scope here.

**Alternative considered**: having init spawn `svcctl` directly so `HANDLE_PARENT` connects them. This doesn't work because users launch `svcctl` from the terminal, not from init. The kernel-side broker is necessary for process discovery.

**Stdout handling for services:**

The `stdout` config field controls how service output is routed:

- `"inherit"` (default): No stdout redirection. Service uses `HANDLE_PARENT` for output (service manager receives it).
- `"log"`: Service manager forwards `Request::Write(Value)` messages from the service's parent channel to `environment::log()` prefixed with the service name.
- `"console"`: Service manager opens `console:/serial/0` and passes it as stdout.
- `"null"`: No stdout handle set. Writes fail silently.

In all cases, the service manager monitors `HANDLE_PARENT` for `Request::Error` and `Request::Warning` messages and always forwards those to the kernel log.

### Main loop

The service manager's event loop handles all events uniformly through a single mailbox:

```
1. Mount filesystems (hardcoded)
2. Scan /config/services/*/config.toml
3. Plan: validate, detect cycles, topological sort → produce initial plan (start all services)
4. Log the plan
5. Register with kernel as `service:/manager` via `OP_SERVICE_REGISTER`, attach broker channel to mailbox
6. Begin executing plan steps
7. Loop:
   a. Execute any plan steps whose prerequisites are satisfied → spawn services
   b. mailbox.recv() — blocks until any event
   c. Dispatch:
      - EVENT_PROCESS_EXITED: record exit, apply restart policy (create restart timer if needed), mark plan step complete, check if new steps are unblocked
      - EVENT_CHANNEL_READABLE (parent channel): forward service log output
      - EVENT_CHANNEL_READABLE (broker channel): accept new connection, perform handshake, attach to mailbox
      - EVENT_CHANNEL_READABLE (command channel): decode ManagerRequest, dispatch, send ManagerResponse
      - EVENT_TIMER (restart timer): re-spawn service
      - EVENT_TIMER (stop grace period): escalate to SIGKILL
      - EVENT_TIMER (stability timer): reset restart counter
8. Never exits (service manager runs for system lifetime)
```

### Data structures

```rust
struct ServiceConfig {
    name: String,
    exec: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    after: Vec<String>,
    restart_policy: RestartPolicy,
    restart_delay_ms: u64,
    restart_max_attempts: u32,
    stdout: StdoutTarget,
    protocol: Option<String>,  // protocol name for discovery
}

enum RestartPolicy { No, OnFailure, Always }
enum StdoutTarget { Inherit, Log, Null, Console }

enum ServiceState {
    Waiting,
    Running {
        handle: Handle,
    },
    Exited {
        code: i32,
    },
    Restarting {
        timer_handle: Handle,
        attempt: u32,
    },
    Stopping {
        handle: Handle,
        timer_handle: Handle,  // grace period timer
    },
    Stopped,
}

struct Service {
    config: ServiceConfig,
    state: ServiceState,
    restart_count: u32,
    stability_timer: Option<Handle>,
}

enum Action {
    Start { service: String },
    Stop { service: String },
    Restart { service: String },
}

struct PlanStep {
    action: Action,
    after: Vec<usize>,  // prerequisite step indices
    complete: bool,
}

struct Plan {
    steps: Vec<PlanStep>,
    warnings: Vec<String>,
}

struct ServiceManagerState {
    services: Vec<Service>,
    name_to_index: BTreeMap<String, usize>,
    current_plan: Option<Plan>,
    mailbox: Mailbox,
    handle_to_index: BTreeMap<u32, usize>,  // process/timer handle → service index
    broker_channel: Handle,                   // receives new connection handles from kernel
    command_channels: Vec<ProtocolChannel<Manager>>,  // typed management connections
}
```

### Filesystem scanning

Init reads `/config/services` using `environment::opendir("file:/config/services")`, iterates subdirectory entries, opens `config.toml` inside each, and parses with the `toml` crate. The syscalls (`OP_ENVIRONMENT_OPENDIR`, file read) and libpanda APIs (`Dir`, `File`) already exist — init just hasn't used them before.

If `/config/services` doesn't exist (e.g., fresh image), init falls back to spawning `file:/mnt/terminal` directly, preserving current behavior.

## Implementation plan

### Phase 1: Signal support

The service manager needs to stop services gracefully. The ABI already reserves `OP_PROCESS_SIGNAL` (`0x2_0004`) and the terminal protocol defines `Signal` variants (`Interrupt`, `Quit`, `Suspend`), but `handle_signal()` in `panda-kernel/src/syscall/process.rs:59` currently returns `-1`.

Catchable signals (SIGTERM, SIGINT, etc.) are delivered as **messages** on the process's `HANDLE_PARENT` channel using the existing terminal protocol: `Event::Signal(Signal)`. This is consistent with how the terminal already sends `Ctrl+C` to children — the service manager is simply another parent that can signal its children the same way. No new kernel mechanism is needed for catchable signals; the parent sends an `Event::Signal` message on the spawn channel.

`SIGKILL` is the exception — it must be unconditional. The kernel forcibly terminates the process (removes from scheduler, sets exit code). This requires `OP_PROCESS_SIGNAL` to handle SIGKILL as a kernel-level operation.

The service manager's stop sequence is: send `Event::Signal(Signal::Terminate)` on the parent channel → start grace timer → if process doesn't exit before timer fires → `process::kill(handle)`. Services that read their `HANDLE_PARENT` channel (which well-behaved services already do for the terminal protocol) will see the SIGTERM and can shut down gracefully. Services that don't will miss it, and the grace timer will escalate to SIGKILL.

**Files:**
- `panda-abi/src/lib.rs` — define `SIGKILL` constant for the kernel-level kill operation
- `panda-abi/src/terminal.rs` — add `Signal::Terminate` variant (value `3`) to the existing `Signal` enum
- `panda-kernel/src/syscall/process.rs` — implement `handle_signal(handle_id, signal)`: for SIGKILL, terminate process immediately (remove from scheduler, set exit code); for other signals, return error (catchable signals go through channels)
- `userspace/libpanda/src/process/mod.rs` — add `process::kill(handle)` public API for SIGKILL

### Phase 2: Timer resource

The service manager needs timed wakeups for restart delays and signal timeouts. Rather than polling, add a **timer resource** that integrates with the existing mailbox event system. The kernel already has the infrastructure:

- The APIC timer fires every 10ms, updating `time::uptime_ms()` and checking deadlines via `DeadlineTracker` (`scheduler/deadline.rs`).
- Kernel tasks can already `sleep_ms()` by registering deadlines (`executor/sleep.rs`).
- Resources post events to mailboxes via `MailboxRef` (used by keyboard, channels, process exit).

A `TimerResource` implements the `Resource` trait. When created with a duration, it registers a deadline with the scheduler. When the APIC timer interrupt finds the deadline expired, it posts `EVENT_TIMER` to the attached mailbox. The timer is one-shot (fires once, then becomes inert).

```rust
let timer = environment::create_timer(delay_ms)?;
mailbox.attach(timer, EVENT_TIMER);
// ... mailbox.recv() returns (timer_handle, EVENT_TIMER) when it fires
```

This eliminates all need for a time syscall in the service manager — timers express "wake me after X ms" directly, which is all the manager needs.

**Files:**
- `panda-abi/src/lib.rs` — add `EVENT_TIMER` constant, `OP_TIMER_CREATE` operation, `HandleType::Timer`
- `panda-kernel/src/resource/timer.rs` — `TimerResource` implementing `Resource`, `attach_mailbox()`, deadline registration
- `panda-kernel/src/syscall/environment.rs` — `handle_create_timer(ms)` creates resource, returns handle
- `panda-kernel/src/scheduler/context_switch.rs` — extend deadline wake to post mailbox events for timer resources
- `userspace/libpanda/src/sys/env.rs` — `create_timer()` raw wrapper
- `userspace/libpanda/src/environment.rs` — `create_timer(ms: u64) -> Result<Handle>` public API

### Phase 3: Service protocol framework

Implement the `Protocol` trait, message framing, handshake, and client/server wrappers in `panda-abi` and `libpanda`. This is foundational infrastructure used by the service scheme (phase 4), the service manager protocol (phase 8), and all future service protocols.

**Files:**
- `panda-abi/src/protocol.rs` — `Protocol` trait, `Service` trait, `MessageKind` enum (`Request`/`Response`/`Event`/`Handshake`), `Hello`/`Welcome`/`Rejected` handshake message types with `Encode`/`Decode` implementations, framing helpers for encoding/decoding the `Kind` byte prefix
- `userspace/libpanda/src/protocol.rs` — `ProtocolChannel<P>` (typed channel wrapper with `send_request`, `send_response`, `send_event`, `recv`, `call`), `Message<P>` enum, handshake initiation/response methods
- `userspace/libpanda/src/service.rs` — `ServiceClient<S>` (connection via `service:/` scheme + automatic handshake), convenience methods

### Phase 4: Service scheme

Add a kernel-side `service:` scheme that brokers channel connections. The path is a flat namespace — `service:/{name}` — where the name maps to a broker channel. The protocol framework from phase 3 provides the handshake that occurs after the channel is established.

1. At boot, after init creates its mailbox, it registers a channel endpoint with the kernel via `OP_SERVICE_REGISTER`. The kernel stores this endpoint in the `service:` scheme handler.
2. When any process opens `service:/manager`, the `ServiceScheme` handler creates a channel pair, sends one endpoint to init via the broker channel, returns the other to the caller.
3. Init attaches each incoming connection to its mailbox. On first message, it performs the protocol handshake — verifying the UUID and negotiating capabilities.

Initially only init registers. The design generalizes to per-process registration for the userspace driver migration.

**Files:**
- `panda-abi/src/lib.rs` — add `OP_SERVICE_REGISTER` operation code
- `panda-kernel/src/resource/service_scheme.rs` — `ServiceScheme` implementing `SchemeHandler`. Stores a `BTreeMap<String, Handle>` mapping service names to broker channel endpoints. On `open("/{name}")`, creates a channel pair, sends one endpoint to the registrant via the broker channel, returns the other to the caller.
- `panda-kernel/src/resource/scheme.rs` — register `service:` scheme in `init()`
- `panda-kernel/src/syscall/environment.rs` — `handle_service_register(name)` creates a channel pair, stores one end in `ServiceScheme` under the given name, returns the other to the calling process
- `userspace/libpanda/src/environment.rs` — add `service_register(name)` API, and `open_service(name)` convenience wrapper around `environment::open("service:/name")`

### Phase 5: TOML parsing and config

**Files:**
- `userspace/init/Cargo.toml` — add `toml = { version = "0.9", default-features = false, features = ["parse"] }`
- `userspace/init/src/config.rs` — `ServiceConfig` struct (including `protocol` field), `parse(name: &str, content: &str) -> Result<ServiceConfig>`, `scan_services(path: &str) -> Result<Vec<ServiceConfig>>`

### Phase 6: Planner

Implement the planning pipeline as a separate module.

The planner is stateless — it takes current state and desired state, returns a plan. It's called both at boot (current = empty) and at runtime (current = whatever's running).

**Files:**
- `userspace/init/src/plan.rs` — `validate()`, `detect_cycles()`, `diff()`, `topological_sort()`, `plan(current_state, desired_configs) -> Result<Plan>`

### Phase 7: Service manager core

**Files:**
- `userspace/init/src/main.rs` — rewritten main loop
- `userspace/init/src/manager.rs` — `ServiceManagerState` with event loop, plan execution, restart/stop/log-forwarding logic

**ServiceManagerState methods:**
```rust
impl ServiceManagerState {
    fn new() -> Self
    fn load_and_plan(&mut self, configs: Vec<ServiceConfig>)
    fn execute_ready_steps(&mut self)
    fn handle_event(&mut self, handle: u32, events: u32)
    fn handle_process_exit(&mut self, handle: u32)
    fn handle_restart_timer(&mut self, handle: u32)
    fn handle_stop_timer(&mut self, handle: u32)
    fn handle_stability_timer(&mut self, handle: u32)
    fn handle_new_connection(&mut self, broker_channel: Handle)
    fn handle_command(&mut self, channel: &ProtocolChannel<Manager>)
    fn handle_service_output(&mut self, handle: u32)
    fn stop_service(&mut self, index: usize)
}
```

Main function:

```rust
libpanda::main! {
    environment::mount("ext2", "/mnt").expect("mount failed");

    let configs = match scan_services("/config/services") {
        Ok(configs) if !configs.is_empty() => configs,
        _ => {
            environment::log("init: no service configs found, spawning terminal directly");
            environment::spawn("file:/mnt/terminal").ok();
            return 0;
        }
    };

    let mut manager = ServiceManagerState::new();
    manager.load_and_plan(configs);

    for warning in &manager.current_plan_warnings() {
        environment::log(&format!("init: {}", warning));
    }

    loop {
        manager.execute_ready_steps();
        let (handle, events) = manager.mailbox.recv();
        manager.handle_event(handle, events);
    }
}
```

### Phase 8: Service manager API crate and `svcctl`

Define the service manager's typed protocol in a shared API crate. Both init and `svcctl` depend on it.

**Files:**
- `userspace/service-manager-api/src/lib.rs` — `Manager` struct implementing `Protocol` + `Service`, `ManagerRequest`/`ManagerResponse`/`ManagerEvent` enums with `Encode`/`Decode`, capability constants. Includes `ListByProtocol` request for protocol-based service discovery.
- `userspace/svcctl/src/main.rs` — CLI tool using `ServiceClient<Manager>`: `svcctl start|stop|restart|status|list|add|remove [name] [options]`
- `userspace/init/src/commands.rs` — `ManagerRequest` dispatch, response encoding via `ProtocolChannel<Manager>`

`svcctl` connects with `ServiceClient::<Manager>::connect(...)`, sends typed `ManagerRequest` variants, receives `ManagerResponse`, and writes the result to `HANDLE_PARENT` (or `HANDLE_STDOUT` in a pipeline). The `List` response contains `Value::Table` for pipeline compatibility.

### Phase 9: Service config files and boot test

Add service definition files to the ext2 image build and verify the system boots with the new service manager.

**Files:**
- `rootfs/config/services/terminal/config.toml`

```toml
[service]
exec = "file:/mnt/terminal"
```

## Testing

- **Protocol framework**: Handshake with matching UUID succeeds. Mismatched UUID returns `Rejected`. Capabilities negotiation returns intersection. `ProtocolChannel` send/recv round-trips request, response, event message kinds correctly. `ServiceClient::connect` performs handshake automatically.
- **Service scheme**: `service_register("manager")` succeeds. `open("service:/manager")` returns a channel. Opening an unregistered name fails.
- **Config parser**: Valid configs, missing `exec`, unknown fields, empty env table, multiple deps, all restart policies, all stdout targets, optional `protocol` field.
- **Planner — boot**: All configs valid, produces Start actions in dependency order.
- **Planner — cycles**: A→B→A cycle detected, reported, excluded. Non-cyclic services still start.
- **Planner — missing deps**: Reference to undefined service reported, service excluded.
- **Planner — runtime add**: Adding a service produces a single Start action.
- **Planner — runtime stop**: Stopping a service with dependents produces Stop actions in reverse dependency order (dependents first).
- **Planner — runtime change**: Changing a running service's config produces Restart action.
- **Backwards compatibility**: No `/config/services/` directory → falls back to spawning terminal.
- **Dependency ordering**: B depends on A → B starts after A.
- **Restart policy**: `restart.policy = "always"` with immediate-exit binary → restarts up to `max_attempts` then stops.
- **Restart delay**: Timer fires after `delay_ms`, not before.
- **Stop with SIGTERM**: Service handles SIGTERM and exits → clean stop.
- **Stop with SIGKILL escalation**: Service ignores SIGTERM → grace timer fires → SIGKILL → forced stop.
- **Protocol-based discovery**: `ListByProtocol` returns services matching a given protocol name.
- **`svcctl` commands**: `list`, `status`, `start`, `stop` produce correct responses. `list | grep running` works via pipeline.

## Risks

1. **`toml` crate in `no_std`**: The `toml` crate v0.9 advertises `no_std` support, but this hasn't been tested in panda's userspace environment. If it doesn't compile cleanly (e.g., transitive `std` dependency), alternatives are `toml_edit` with `default-features = false` or a hand-written parser for the TOML subset we use (string values, string arrays, tables — no inline tables, datetimes, or multiline strings).
