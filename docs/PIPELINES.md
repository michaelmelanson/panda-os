# Structured Pipelines

Panda OS supports shell pipelines (`cmd1 | cmd2 | cmd3`) where tools exchange structured `Value` objects rather than raw bytes. This is similar to PowerShell's object pipeline while maintaining Unix compatibility through `Value::String` and `Value::Bytes` variants.

## Control Plane vs Data Plane

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

## Pipeline Topology

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

## Value Type

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

Key design points:
- `Styled` wraps any `Value` for formatting
- `Table` enforces rectangular structure
- `Map` provides JSON-like structured data
- Unix compatibility via `String` and `Bytes` variants

## Protocol Messages

### Control Plane (Request/Event)

**Request (program -> terminal via PARENT):**
```rust
enum Request {
    Error(Value),
    Warning(Value),
    RequestInput(InputRequest),
    Query(Query),
    Progress { current: u32, total: u32, message: String },
    SetTitle(String),
    MoveCursor { row: u16, col: u16 },
    Clear(ClearRegion),
    Exit(i32),
}
```

**Event (terminal -> program via PARENT):**
```rust
enum Event {
    InputResponse(InputResponse),
    Key(KeyEvent),
    Resize { cols: u16, rows: u16 },
    Signal(Signal),
    QueryResponse(QueryResponse),
}
```

## Handle Layout

```rust
HANDLE_STDIN  = 0    // Data input from previous pipeline stage
HANDLE_STDOUT = 1    // Data output to next pipeline stage  
HANDLE_STDERR = 2    // Reserved for future error stream
HANDLE_PROCESS     = 3    // Current process resource
HANDLE_ENVIRONMENT = 4    // System environment
HANDLE_MAILBOX     = 5    // Default mailbox
HANDLE_PARENT      = 6    // Control channel to parent (terminal)
```

## Execution Model

### Standalone Execution (no pipeline)

```rust
// STDIN/STDOUT are invalid (not set)
// Program uses PARENT for both data and control

fn main() {
    // Data output: send Value via PARENT (terminal renders it)
    println!("Hello, world!");
    
    // Control: request input via PARENT
    let input = terminal::input("Enter name: ");
}
```

### Pipeline Execution

```rust
// STDIN/STDOUT are valid (connected to adjacent stages)
// PARENT is for control only

fn main() {
    // Data input: receive Value from STDIN
    let input: Value = stdio::read_value();
    
    // Process...
    let output = transform(input);
    
    // Data output: send Value to STDOUT
    stdio::write_value(output);
    
    // Control: report error via PARENT (reaches terminal directly)
    terminal::error("something went wrong");
}
```

## Examples

### `ls | grep foo`

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

`grep` can understand `Table` and filter rows, or fall back to text filtering.

### Error reporting from middle stage

```
cmd1 | cmd2 | cmd3

cmd2 encounters an error:
- Sends Request::Error("something went wrong") via PARENT
- Terminal displays immediately
- Data flow to cmd3 continues independently
```

## Terminal's Role

The terminal:
1. Spawns all pipeline processes with PARENT channels to itself
2. Creates data channels connecting STDOUT[n] -> STDIN[n+1]
3. Receives and renders `Value` from the final stage's STDOUT
4. Handles `Request` messages from any stage (errors, input prompts, etc.)
5. Sends `Event` messages to processes as needed
