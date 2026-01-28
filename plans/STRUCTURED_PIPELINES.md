# Structured Pipelines via Value Objects

## Overview

Enable shell pipelines (`cmd1 | cmd2 | cmd3`) where tools exchange structured `Value` objects rather than raw bytes. This is more like PowerShell's object pipeline than Unix's byte streams, while maintaining Unix compatibility through `Value::String` and `Value::Bytes` variants.

---

## Design Philosophy

### Control Plane vs Data Plane

The architecture separates two types of IPC:

**Control Plane (`HANDLE_PARENT`):**
- Interactive terminal features: input prompts, queries, cursor control
- Error/warning messages that must reach the user
- Progress reporting
- Always connects to parent process (terminal/shell)
- Each process in a pipeline has its own PARENT channel

**Data Plane (`HANDLE_STDIN` / `HANDLE_STDOUT`):**
- Structured `Value` objects flowing through pipelines
- Normal program output
- Only set when process is spawned as part of a pipeline
- Falls back to PARENT when not in a pipeline (standalone execution)

### Pipeline Topology

```
terminal <--PARENT--> cmd1 ==STDOUT==> ==STDIN==> cmd2 ==STDOUT==> ==STDIN==> cmd3 <--PARENT--> terminal
              ^                (Data)                    (Data)                          ^
              |                                                                          |
              +------------------------------- PARENT -----------------------------------+
                            (each process has independent control channel)
```

- Data flows left-to-right through STDIN/STDOUT channels
- Control messages flow directly between each process and terminal
- Middle stages can report errors/progress without disrupting data flow

### Structured Value Objects

Tools exchange `Value` objects - a universal type for all pipeline data:

```rust
enum Value {
    // Primitives
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),         // Text (Unix compatibility)
    Bytes(Vec<u8>),         // Raw binary (Unix compatibility)
    
    // Containers
    Array(Vec<Value>),
    Map(BTreeMap<String, Value>),
    
    // Display modifiers (recursive)
    Styled(Style, Box<Value>),
    Link { url: String, inner: Box<Value> },
    
    // Structured display
    Table(Table),
}

struct Table {
    cols: u16,
    headers: Option<Vec<Value>>,  // length must equal cols (if Some)
    cells: Vec<Value>,            // length must be multiple of cols
}
```

**Key design points:**
- `Styled` wraps any `Value`, enabling `Value::Styled(Style::bold(), Box::new(Value::String("important")))`
- `Link` similarly wraps any `Value` for the link text
- `Table` enforces rectangular structure: `cells.len() % cols == 0`
- Headers are optional and separate from data rows
- `Map` provides JSON-like structured data without text encoding overhead

**Unix compatibility:** Tools can always fall back to `Value::String` or `Value::Bytes` for traditional byte-stream behavior. A tool receiving structured data it doesn't understand can convert to string.

### Example: `ls | grep foo`

```
ls                              grep foo                        terminal
 |                                |                               |
 +-> Value::Table(Table {      ->+-> Value::Table(Table {      ->+-> renders table
       cols: 2,                 |      cols: 2,                 |
       headers: ["Name","Size"],|      headers: ["Name","Size"],|
       cells: [                 |      cells: [                 |
         "foo.txt", 1024,       |        "foo.txt", 1024,       |
         "bar.txt", 2048,       |        "foobar", 512,         |
         "foobar", 512,         |      ]                        |
       ]                        |    })                         |
     })                         |                               |
```

`grep` can:
1. Understand `Table` and filter rows where any cell contains "foo"
2. Convert `Table` to text lines and filter traditionally
3. Output filtered `Table` (preserving structure) or `String` (traditional)

### Example: `cat file.json | jq '.name'`

```
cat                             jq                              terminal
 |                                |                               |
 +-> Value::Map({...})         ->+-> Value::String("value")    ->+-> renders text
     or Value::String("{...}")  |    or Value::Map({...})       |
```

If `cat` detects JSON, it can parse and emit `Value::Map`. `jq` processes it and outputs result. No JSON text re-encoding between stages.

### Example: Error reporting from middle stage

```
cmd1 | cmd2 | cmd3

cmd2 encounters an error:
- Sends Request::Error("something went wrong") via PARENT -> terminal displays immediately
- Can continue processing or exit
- Data flow to cmd3 is independent of error reporting
```

---

## Protocol Definitions

### Data Plane Messages

`Value` objects flow through STDIN/STDOUT:

```rust
/// Universal value type for pipeline data.
enum Value {
    // Primitives
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    
    // Containers
    Array(Vec<Value>),
    Map(BTreeMap<String, Value>),
    
    // Display modifiers
    Styled(Style, Box<Value>),
    Link { url: String, inner: Box<Value> },
    
    // Structured display
    Table(Table),
}

/// Rectangular table with optional headers.
struct Table {
    cols: u16,
    headers: Option<Vec<Value>>,  // length == cols if Some
    cells: Vec<Value>,            // length % cols == 0
}
```

### Control Plane Messages

**Request (program -> terminal via PARENT):**

```rust
/// Control messages from program to terminal.
/// Renamed from `TerminalOutput` to clarify it's not data output.
enum Request {
    // Error/warning display (always shown, even from middle pipeline stages)
    Error(Value),
    Warning(Value),
    
    // Interactive features
    RequestInput(InputRequest),
    Query(Query),
    
    // UI control
    Progress { current: u32, total: u32, message: String },
    SetTitle(String),
    MoveCursor { row: u16, col: u16 },
    Clear(ClearRegion),
    
    // Lifecycle
    Exit(i32),
}
```

**Event (terminal -> program via PARENT):**

```rust
/// Control messages from terminal to program.
/// Renamed from `TerminalInput` to clarify it's not data input.
enum Event {
    /// Response to RequestInput
    InputResponse(InputResponse),
    /// Raw key event (when in RawKeys mode)
    Key(KeyEvent),
    /// Terminal resized
    Resize { cols: u16, rows: u16 },
    /// Signal from user (Ctrl+C, etc.)
    Signal(Signal),
    /// Response to Query
    QueryResponse(QueryResponse),
}
```

### Key Changes from Current Protocol

| Current | New | Rationale |
|---------|-----|-----------|
| `Output` | `Value` | Universal type for all pipeline data |
| `Output::Text/Styled/Table/...` | `Value::String/Styled/Table/...` | Unified under Value with primitives |
| `Output::Json(String)` | `Value::Map` | Binary encoding, no JSON text overhead |
| `StyledText` | `Value::Styled(Style, Box<Value>)` | Recursive wrapper, not separate type |
| `TerminalOutput` | `Request` | It's a request to the terminal, not output |
| `TerminalOutput::Write(Output)` | Removed | Data goes through STDOUT, not control plane |
| `TerminalInput` | `Event` | It's an event from terminal, not input data |
| - | `Request::Error/Warning` | New: side-band error reporting for pipelines |

---

## Handle Layout

```rust
// Data plane (pipeline I/O)
HANDLE_STDIN  = 0    // Data input from previous pipeline stage
HANDLE_STDOUT = 1    // Data output to next pipeline stage  
HANDLE_STDERR = 2    // Reserved for future error stream

// Control plane  
HANDLE_PROCESS     = 3    // Current process resource
HANDLE_ENVIRONMENT = 4    // System environment
HANDLE_MAILBOX     = 5    // Default mailbox
HANDLE_PARENT      = 6    // Control channel to parent (terminal)
```

---

## Execution Model

### Standalone Execution (no pipeline)

```rust
// STDIN/STDOUT are invalid (not set)
// Program uses PARENT for both data and control

fn main() {
    // Data output: send Value via PARENT (terminal renders it)
    send_value(Handle::PARENT, Value::String("Hello, world!".into()));
    
    // Control: request input via PARENT
    send_request(Handle::PARENT, Request::RequestInput(...));
}
```

The terminal receives `Value` objects on the parent channel and renders them.

### Pipeline Execution

```rust
// STDIN/STDOUT are valid (connected to adjacent stages)
// PARENT is for control only

fn main() {
    // Data input: receive Value from STDIN
    let input: Value = recv_value(Handle::STDIN);
    
    // Process...
    let output = transform(input);
    
    // Data output: send Value to STDOUT
    send_value(Handle::STDOUT, output);
    
    // Control: report error via PARENT (reaches terminal directly)
    send_request(Handle::PARENT, Request::Error(...));
}
```

### Terminal's Role

The terminal:
1. Spawns all pipeline processes with PARENT channels to itself
2. Creates data channels connecting STDOUT[n] -> STDIN[n+1]
3. Receives and renders `Value` from the final stage's STDOUT
4. Handles `Request` messages from any stage (errors, input prompts, etc.)
5. Sends `Event` messages to processes as needed

---

## Migration Strategy

### Backwards Compatibility

- Existing programs using `terminal::print()` etc. continue to work (sends via PARENT)
- Programs not in pipelines see no change (STDIN/STDOUT invalid, use PARENT)
- `Value::String` and `Value::Bytes` provide Unix-style compatibility

### Deprecation

- `TerminalOutput::Write(Output)` removed - use STDOUT for data
- `Output`, `StyledText`, `StyledSpan` removed - use `Value` and `Value::Styled`
- Direct `terminal::print()` for data output deprecated in favor of `print!` macro
- `terminal::*` functions reserved for control plane operations
