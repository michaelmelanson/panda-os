# System initialization tool

## Problem

The `init` process (`userspace/init/src/main.rs`) is a hardcoded Rust binary that mounts ext2 at `/mnt` and spawns the terminal. Adding or reordering services requires recompiling. There is no way to declare dependencies between services, restart crashed services, or configure service properties (environment, stdio) without changing code. There is also no mechanism to manage services at runtime — stopping, starting, or adding services after boot.

## Goal

Build a service manager that reads declarative TOML service configurations, computes a plan (a DAG of actions) to bring the system into the desired state, and executes it. The service manager continues running as a supervisor — monitoring services, restarting them according to policy, and accepting runtime commands to start, stop, add, and remove services. Runtime changes go through the same planning pipeline: compute a new plan from the delta between current state and desired state, then execute it.

## Constraints

- **Filesystem is read-only (ext2 has no write support).** Service configs are baked into the ext2 image at build time. Runtime service additions come via IPC commands.
- **Max message size is 4096 bytes.** Channel messages (startup, commands, responses) are bounded by this.
- **Userspace is `no_std` + `alloc`.** Any dependencies must work without `std`.

## Design

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

[service.env]                  # Optional. Environment variables.
KEY = "value"

[dependencies]
after = ["service-a"]          # Optional. Services that must start first. Default: []

[restart]
policy = "no"                  # Optional. "no", "on-failure", "always". Default: "no"
delay_ms = 1000                # Optional. Delay between restart attempts. Default: 1000
max_attempts = 10              # Optional. Max consecutive restarts before giving up. Default: 10
```

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

### IPC architecture: control plane and data plane

Following the panda IPC conventions (see [docs/IPC.md](../docs/IPC.md) and [docs/PIPELINES.md](../docs/PIPELINES.md)), the service manager uses the **control plane** (`HANDLE_PARENT` channel) for communication with managed services, and a separate **command channel** for management commands from tools like `svcctl`.

**Service manager ↔ managed services:**

Each spawned service gets a `HANDLE_PARENT` channel back to the service manager. This is the standard parent-child channel created by `environment::spawn()`. The service manager uses it for:

- Sending the startup message (args + env) per the existing startup protocol
- Receiving structured log output from services that use `Request::Write(Value)` or `Request::Error(Value)` on their parent channel (following the terminal protocol pattern from PIPELINES.md)
- Sending `Event::Signal(Signal)` to request graceful shutdown (same protocol the terminal uses to signal Ctrl+C to children)

This means services don't need special awareness of the service manager — they use the same `HANDLE_PARENT` protocol they'd use with a terminal. The service manager acts as a minimal terminal protocol peer for its children.

**Service manager ↔ management tools (`svcctl`):**

All resource schemes are currently kernel-side only — there is no mechanism for a userspace process to register itself as a scheme handler. To allow arbitrary processes (like `svcctl`) to connect to the service manager, add a **kernel-side `service:` scheme** that brokers channel connections to the init process.

**How it works:**

1. At boot, after init creates its mailbox, it registers a channel endpoint with the kernel via a new syscall `OP_SERVICE_REGISTER`. The kernel stores this endpoint in the `service:` scheme handler.
2. When any process opens `service:/manager`, the kernel-side `ServiceScheme` handler creates a new channel pair, sends one endpoint to init (via the registered channel — init receives it as a message containing the new handle), and returns the other endpoint to the caller.
3. Init attaches each incoming connection to its mailbox with `EVENT_CHANNEL_READABLE`.

This is a minimal naming service — init registers once, and the kernel brokers connections. The pattern could later be generalized to let any process register named services, but for now only init uses it.

**Alternative considered**: having init spawn `svcctl` directly so `HANDLE_PARENT` connects them. This doesn't work because users launch `svcctl` from the terminal, not from init. The kernel-side broker is necessary for process discovery.

Commands and responses use `Value` encoding for consistency with the pipeline system:

```rust
// Command (svcctl → service manager via channel)
Value::Map({
    "action": Value::String("start"),
    "name": Value::String("networkd"),
})

// Command: add a new service at runtime
Value::Map({
    "action": Value::String("add"),
    "name": Value::String("new-daemon"),
    "config": Value::String("...TOML content..."),
})

// Response (service manager → svcctl via channel)
Value::Map({
    "ok": Value::Bool(true),
    "message": Value::String("service started"),
})

// Status query response
Value::Map({
    "ok": Value::Bool(true),
    "name": Value::String("networkd"),
    "state": Value::String("running"),
    "restarts": Value::Int(0),
})

// List response — uses Table for structured display in pipelines
Value::Table(Table {
    cols: 3,
    headers: Some(["Name", "State", "Restarts"]),
    cells: [
        "terminal", "running", 0,
        "networkd", "running", 0,
        "logger",   "exited",  3,
    ],
})
```

Using `Value` means `svcctl list | grep running` works out of the box with the structured pipeline system — `grep` can filter `Table` rows.

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
      - EVENT_CHANNEL_READABLE (command channel): parse command, run planner to compute delta plan, begin executing new plan steps
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

struct ServiceManager {
    services: Vec<Service>,
    name_to_index: BTreeMap<String, usize>,
    current_plan: Option<Plan>,
    mailbox: Mailbox,
    handle_to_index: BTreeMap<u32, usize>,  // process/timer handle → service index
    broker_channel: Handle,                   // receives new connection handles from kernel
    command_channels: Vec<Handle>,             // open management connections
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

### Phase 3: Service scheme

All resource schemes are currently kernel-side only — there is no mechanism for a userspace process to register itself as a scheme handler. To allow arbitrary processes (like `svcctl`) to connect to the service manager, add a **kernel-side `service:` scheme** that brokers channel connections to the init process.

1. At boot, after init creates its mailbox, it registers a channel endpoint with the kernel via a new syscall `OP_SERVICE_REGISTER`. The kernel stores this endpoint in the `service:` scheme handler.
2. When any process opens `service:/manager`, the kernel-side `ServiceScheme` handler creates a new channel pair, sends one endpoint to init (via the registered channel — init receives it as a message containing the new handle), and returns the other endpoint to the caller.
3. Init attaches each incoming connection to its mailbox with `EVENT_CHANNEL_READABLE`.

This is a minimal naming service — init registers once, and the kernel brokers connections. The pattern could later be generalized to let any process register named services, but for now only init uses it.

**Files:**
- `panda-abi/src/lib.rs` — add `OP_SERVICE_REGISTER` operation code
- `panda-kernel/src/resource/service_scheme.rs` — `ServiceScheme` implementing `SchemeHandler`. Stores the init-side broker channel endpoint. On `open("/manager")`, creates a channel pair, sends one endpoint to init as a message on the broker channel, returns the other to the caller.
- `panda-kernel/src/resource/scheme.rs` — register `service:` scheme in `init()`
- `panda-kernel/src/syscall/environment.rs` — `handle_service_register()` creates a channel pair, stores one end in `ServiceScheme`, returns the other to the calling process (init)
- `userspace/libpanda/src/environment.rs` — add `service_register()` API for init, and `open_service(name)` convenience wrapper around `environment::open("service:/name")`

### Phase 4: TOML parsing and config

**Files:**
- `userspace/init/Cargo.toml` — add `toml = { version = "0.9", default-features = false, features = ["parse"] }`
- `userspace/init/src/config.rs` — `ServiceConfig` struct, `parse(name: &str, content: &str) -> Result<ServiceConfig>`, `scan_services(path: &str) -> Result<Vec<ServiceConfig>>`

### Phase 5: Planner

Implement the planning pipeline as a separate module.

The planner is stateless — it takes current state and desired state, returns a plan. It's called both at boot (current = empty) and at runtime (current = whatever's running).

**Files:**
- `userspace/init/src/plan.rs` — `validate()`, `detect_cycles()`, `diff()`, `topological_sort()`, `plan(current_state, desired_configs) -> Result<Plan>`

### Phase 6: Service manager core

**Files:**
- `userspace/init/src/main.rs` — rewritten main loop
- `userspace/init/src/manager.rs` — `ServiceManager` with event loop, plan execution, restart/stop/log-forwarding logic

**ServiceManager methods:**
```rust
impl ServiceManager {
    fn new() -> Self
    fn load_and_plan(&mut self, configs: Vec<ServiceConfig>)
    fn execute_ready_steps(&mut self)
    fn handle_event(&mut self, handle: u32, events: u32)
    fn handle_process_exit(&mut self, handle: u32)
    fn handle_restart_timer(&mut self, handle: u32)
    fn handle_stop_timer(&mut self, handle: u32)
    fn handle_stability_timer(&mut self, handle: u32)
    fn handle_command(&mut self, channel: Handle)
    fn handle_service_output(&mut self, handle: u32)
    fn stop_service(&mut self, index: usize)
    fn apply_command(&mut self, command: Command)
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

    let mut manager = ServiceManager::new();
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

### Phase 7: Service config files and boot test

Add service definition files to the ext2 image build and verify the system boots with the new service manager.

**Files:**
- `rootfs/config/services/terminal/config.toml`

```toml
[service]
exec = "file:/mnt/terminal"
```

### Phase 8: `svcctl` command-line tool

**Files:**
- `userspace/svcctl/src/main.rs` — CLI tool: `svcctl start|stop|restart|status|list|add [name] [options]`
- `userspace/init/src/commands.rs` — command parsing (Value-based), dispatch, response encoding
- `userspace/init/src/manager.rs` — add `handle_command()`, `apply_command()`. On receiving a new connection handle from the broker channel, attach it to the mailbox with `EVENT_CHANNEL_READABLE`.

`svcctl` opens `service:/manager`, sends a `Value::Map` command, receives a `Value` response, and writes it to `HANDLE_PARENT` (or `HANDLE_STDOUT` in a pipeline). Because responses use `Value::Table`, pipeline integration works automatically.

## Testing

- **Config parser**: Valid configs, missing `exec`, unknown fields, empty env table, multiple deps, all restart policies, all stdout targets.
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
- **`svcctl` commands**: `list`, `status`, `start`, `stop` produce correct responses. `list | grep running` works via pipeline.

## Risks

1. **`toml` crate in `no_std`**: The `toml` crate v0.9 advertises `no_std` support, but this hasn't been tested in panda's userspace environment. If it doesn't compile cleanly (e.g., transitive `std` dependency), alternatives are `toml_edit` with `default-features = false` or a hand-written parser for the TOML subset we use (string values, string arrays, tables — no inline tables, datetimes, or multiline strings).
