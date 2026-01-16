# kaish (会sh) Architecture

## 核 (Kaku) — Kernel-First Design

The core insight: **the 核 (kaku/kernel) is the unit of execution**, not the REPL or script.
Frontends (REPL, script runner, MCP server) connect to kernels.

```
┌─────────────────────────────────────────────────────────────────────┐
│                          Frontends                                  │
│  ┌─────────┐  ┌─────────┐  ┌─────────┐  ┌─────────────────────────┐ │
│  │  REPL   │  │ Script  │  │   MCP   │  │     Kaijutsu /          │ │
│  │         │  │ Runner  │  │ Server  │  │     Embedded            │ │
│  └────┬────┘  └────┬────┘  └────┬────┘  └───────────┬─────────────┘ │
└───────┼────────────┼────────────┼───────────────────┼───────────────┘
        │            │            │                   │
        │      KernelClient (trait)                   │
        │   (direct / IPC / HTTP / embedded)          │
        └────────────┴─────┬──────┴───────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────────────────┐
│                    会sh 核 (Kaku) — Kernel                          │
│  ┌────────────────────────────────────────────────────────────────┐ │
│  │ State: variables, tool definitions, VFS mounts, job handles   │ │
│  └────────────────────────────────────────────────────────────────┘ │
│                           │                                         │
│                           ▼                                         │
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │                    Shell Engine                                 ││
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────────┐   │
│  │    Lexer     │  │    Parser    │  │       Interpreter        │   │
│  │   (logos)    │  │   (chumsky)  │  │   (async, tokio-based)   │   │
│  └──────┬───────┘  └──────┬───────┘  └──────────┬───────────────┘   │
│         │                 │                     │                   │
│         ▼                 ▼                     ▼                   │
│  ┌─────────────────────────────────────────────────────────────┐    │
│  │                        AST Types                            │    │
│  │  Command | Pipeline | Redirect | Assignment | ToolDef | ... │    │
│  └─────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────┘
                           │
         ┌─────────────────┼─────────────────┐
         │                 │                 │
         ▼                 ▼                 ▼
┌─────────────────┐ ┌─────────────┐ ┌─────────────────┐
│   Tool Registry │ │     VFS     │ │  Job Scheduler  │
│                 │ │             │ │                 │
│ ┌─────────────┐ │ │ ┌─────────┐ │ │ ┌─────────────┐ │
│ │  Builtins   │ │ │ │ Memory  │ │ │ │ Background  │ │
│ │ echo,ls,cd..│ │ │ │ /scratch│ │ │ │   Jobs      │ │
│ └─────────────┘ │ │ └─────────┘ │ │ └─────────────┘ │
│ ┌─────────────┐ │ │ ┌─────────┐ │ │ ┌─────────────┐ │
│ │ MCP Clients │ │ │ │ LocalFs │ │ │ │  散/集      │ │
│ │ exa, fs, ...│ │ │ │  /src   │ │ │ │  Scatter/   │ │
│ └─────────────┘ │ │ └─────────┘ │ │ │   Gather    │ │
│ ┌─────────────┐ │ │ ┌─────────┐ │ │ └─────────────┘ │
│ │ User Tools  │ │ │ │MCP Res. │ │ │ ┌─────────────┐ │
│ │ (tool def)  │ │ │ │  /mcp/* │ │ │ │   Pipes     │ │
│ └─────────────┘ │ │ └─────────┘ │ │ │  (channels) │ │
└─────────────────┘ └─────────────┘ │ └─────────────┘ │
                                    └─────────────────┘
```

## Crate Structure

```
kaish/
├── Cargo.toml
├── schema/
│   └── kaish.capnp             # Cap'n Proto schema (source of truth)
│
├── crates/
│   ├── kaish-schema/           # Generated Cap'n Proto code
│   │   ├── Cargo.toml
│   │   ├── build.rs            # capnpc code generation
│   │   └── src/
│   │       └── lib.rs          # Re-exports generated types
│   │
│   ├── kaish-kernel/           # Core kernel (the heart)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs          # Kernel public API
│   │       ├── kernel.rs       # Kernel state & lifecycle
│   │       ├── lexer/
│   │       │   ├── mod.rs
│   │       │   └── tokens.rs   # Token definitions (logos)
│   │       ├── parser/
│   │       │   ├── mod.rs
│   │       │   ├── ast.rs      # AST types
│   │       │   └── error.rs    # Parse errors with spans
│   │       ├── interpreter/
│   │       │   ├── mod.rs
│   │       │   ├── eval.rs     # Expression evaluation
│   │       │   ├── exec.rs     # Command execution
│   │       │   └── result.rs   # The $? result type
│   │       ├── tools/
│   │       │   ├── mod.rs
│   │       │   ├── registry.rs # Tool lookup & dispatch
│   │       │   ├── builtin/    # Built-in tools
│   │       │   └── mcp.rs      # MCP client wrapper
│   │       ├── vfs/
│   │       │   ├── mod.rs
│   │       │   ├── traits.rs   # Filesystem trait
│   │       │   ├── memory.rs   # In-memory (/scratch)
│   │       │   ├── local.rs    # Local filesystem
│   │       │   └── router.rs   # Mount point routing
│   │       └── scheduler/
│   │           ├── mod.rs
│   │           ├── job.rs      # Job state & handles
│   │           ├── scatter.rs  # Scatter implementation (会!)
│   │           └── gather.rs   # Gather implementation
│   │
│   ├── kaish-client/           # Client trait + implementations
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── traits.rs       # KernelClient trait
│   │       ├── embedded.rs     # Direct in-process kernel
│   │       └── ipc.rs          # Unix socket connection
│   │
│   ├── kaish-repl/             # Interactive REPL frontend
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── readline.rs     # Line editing (rustyline)
│   │       ├── completer.rs    # Tab completion
│   │       └── expansion.rs    # YAML→JSON expansion on Tab
│   │
│   └── kaish-mcp/              # MCP server frontend
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── server.rs       # MCP protocol handler
│           └── export.rs       # Tool def → MCP schema
│
├── src/
│   └── main.rs                 # CLI binary (kaish command)
│
└── tests/
    ├── lexer_tests.rs
    ├── parser_tests.rs
    └── integration/
        └── *.kai           # Script-based tests
```

## 核 Architecture

### Kernel State

```rust
pub struct Kernel {
    /// Variable bindings (scoped)
    variables: Scope,

    /// User-defined tools from `tool` statements
    user_tools: HashMap<String, ToolDef>,

    /// Registered MCP servers
    mcp_clients: HashMap<String, Box<dyn McpClient>>,

    /// Virtual filesystem with mount points
    vfs: VfsRouter,

    /// Background jobs
    jobs: JobManager,

    /// Last command result ($?)
    last_result: ExecResult,
}
```

### 核 Protocol (Cap'n Proto)

The kernel protocol is defined in `schema/kaish.capnp`. Cap'n Proto gives us:
- **Zero-copy reads** - no deserialization overhead for IPC
- **Streaming RPC** - built-in support for output streaming
- **Schema evolution** - add fields without breaking clients
- **Capability-based security** - natural fit for tool permissions

```capnp
interface Kernel {
  # Execution
  execute @0 (input :Text) -> (result :ExecResult);
  executeStreaming @1 (input :Text) -> (stream :OutputStream);

  # Variables
  getVar @2 (name :Text) -> (value :Value);
  setVar @3 (name :Text, value :Value) -> ();
  listVars @4 () -> (vars :List(KeyValue));

  # Tools
  listTools @5 () -> (tools :List(ToolInfo));
  callTool @7 (name :Text, args :List(KeyValue)) -> (result :ExecResult);

  # Jobs
  listJobs @8 () -> (jobs :List(JobInfo));
  cancelJob @9 (id :UInt64) -> (success :Bool);

  # State persistence
  snapshot @17 () -> (state :KernelState);
  restore @18 (state :KernelState) -> ();

  # Lifecycle
  ping @20 () -> (pong :Text);
  shutdown @21 () -> ();
}
```

### KernelClient Implementations

```rust
// Generated from kaish.capnp
use kaish_schema::kernel_capnp::kernel;

// === Implementations ===

/// Direct in-process kernel (Kaijutsu uses this)
/// Bypasses serialization entirely for performance
pub struct EmbeddedClient {
    kernel: Arc<RwLock<Kernel>>,
}

/// Connect to kernel over Unix socket via Cap'n Proto RPC
pub struct IpcClient {
    client: kernel::Client,
    connection: RpcConnection,
}
```

### 核 Lifecycle

```
┌─────────────────────────────────────────────────────────────────────┐
│                      Kernel Lifecycle                               │
│                                                                     │
│  ┌─────────┐    ┌─────────┐    ┌─────────┐    ┌─────────┐          │
│  │  New    │───▶│  Init   │───▶│ Running │───▶│ Shutdown│          │
│  │         │    │         │    │         │    │         │          │
│  └─────────┘    └─────────┘    └─────────┘    └─────────┘          │
│                      │              │                               │
│                      │              │                               │
│                      ▼              ▼                               │
│              Load config      Execute statements                   │
│              Mount VFS        Run background jobs                  │
│              Register MCP     Handle tool calls                    │
│              Load tools.kai   Scatter/gather                       │
└─────────────────────────────────────────────────────────────────────┘
```

### Usage Patterns

```rust
// === Embedded (Kaijutsu) ===
let kernel = Kernel::new();
kernel.mount("/workspace", local_fs);
kernel.register_mcp("exa", exa_client);

let client = EmbeddedClient::new(kernel);
let result = client.execute("ls /workspace").await?;

// === REPL with persistent kernel ===
let client = IpcClient::connect("/tmp/kaish.sock")?;
// Kernel process manages its own lifecycle

// === MCP Server ===
let kernel = Kernel::new();
kernel.load_file("tools.kai").await?;  // define tools
McpServer::new(kernel).serve_stdio().await?;

// === Script Runner ===
let kernel = Kernel::new();
let client = EmbeddedClient::new(kernel);
let script = fs::read_to_string("script.kai")?;
client.execute(&script).await?;
```

## State Storage (SQLite)

Kernel state is persisted in SQLite. Why SQLite?

| Feature | Benefit |
|---------|---------|
| ACID transactions | Atomic updates, no corrupt state |
| WAL mode | Crash recovery, concurrent reads |
| Incremental updates | Change one variable, not full snapshot |
| Query without full load | "List tools" doesn't load all vars |
| Blob storage | Large values stored efficiently |
| Battle-tested | 20+ years of reliability |

Build time: ~20-30s extra on cold build (compiles bundled SQLite), cached after.

### Schema Overview

```sql
-- Core tables
variables     -- name, value_type, value_small/value_blob
tools         -- name, source, params_json
mounts        -- path, backend_type, config_json, read_only
mcp_servers   -- name, transport_type, config_json

-- Runtime state
last_result   -- $? (code, ok, err, stdout, stderr, data)
cwd           -- current working directory

-- Metadata
meta          -- schema_version, session_id, created_at
```

See `schema/state.sql` for full DDL.

### Usage

```rust
// Kernel owns the SQLite connection
pub struct Kernel {
    db: rusqlite::Connection,  // In WAL mode
    // ...
}

// Set a variable (incremental, not full snapshot)
kernel.set_var("X", Value::Int(42))?;
// Executes: INSERT OR REPLACE INTO variables ...

// Get a variable
let val = kernel.get_var("X")?;
// Executes: SELECT value_type, value_small FROM variables WHERE name = ?

// Export full state (for transfer/backup)
let json = kernel.export_state_json()?;
// Uses the state_export view

// Clone state to new kernel
let state = kernel_a.export_state()?;
kernel_b.import_state(state)?;
```

### Large Values (Blobs)

Values > 1KB are stored as blobs and streamed:

```rust
// Small value: stored inline
kernel.set_var("small", Value::String("hello".into()))?;

// Large value: stored as blob, returns reference
let blob_id = kernel.write_blob(large_data, "application/octet-stream")?;
kernel.set_var("large", Value::Blob(BlobRef { id: blob_id, ... }))?;

// Reading large value streams it
let stream = kernel.read_blob(&blob_id)?;
while let Some(chunk) = stream.next().await? {
    // process chunk
}
```

### State File Location

```
~/.local/share/kaish/
├── kernels/
│   ├── default.db           # Default kernel state
│   ├── project-foo.db       # Named kernel
│   └── session-abc123.db    # Ephemeral session
└── blobs/
    └── <sha256-prefix>/     # Blob storage (content-addressed)
```

## 核 Discovery (Socket Files)

Kernels are discovered via socket files:

```
/tmp/kaish-$USER/
├── default.sock             # Default kernel
├── default.pid              # PID for stale detection
├── project-foo.sock         # Named kernel
└── session-abc123.sock      # Ephemeral session
```

### Socket File Protocol

```rust
// Kernel creates socket on startup
let socket_path = format!("/tmp/kaish-{}/{}.sock", user, kernel_name);
let listener = UnixListener::bind(&socket_path)?;

// Write pid file for cleanup detection
fs::write(format!("{}.pid", socket_path), std::process::id().to_string())?;

// Client connects
let stream = UnixStream::connect(&socket_path)?;
let client = capnp_rpc::new_client(stream);
```

### Stale Socket Cleanup

```rust
fn cleanup_stale_sockets(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension() == Some("sock") {
            let pid_file = path.with_extension("pid");
            if let Ok(pid_str) = fs::read_to_string(&pid_file) {
                let pid: u32 = pid_str.trim().parse().unwrap_or(0);
                if !process_exists(pid) {
                    fs::remove_file(&path)?;
                    fs::remove_file(&pid_file)?;
                }
            }
        }
    }
    Ok(())
}
```

### REPL 核 Connection

```rust
// REPL startup logic
fn connect_to_kernel(name: Option<&str>) -> Result<KernelClient> {
    let socket_dir = format!("/tmp/kaish-{}/", whoami::username());
    cleanup_stale_sockets(&socket_dir)?;

    let socket_name = name.unwrap_or("default");
    let socket_path = format!("{}{}.sock", socket_dir, socket_name);

    if Path::new(&socket_path).exists() {
        // Connect to existing kernel
        IpcClient::connect(&socket_path)
    } else {
        // Start new kernel
        let kernel = Kernel::new(&format!("{}{}.db", data_dir(), socket_name))?;
        let listener = kernel.listen(&socket_path)?;

        // Fork to background or return embedded client
        EmbeddedClient::new(kernel)
    }
}
```

## Key Types

```rust
// === AST ===

pub enum Stmt {
    Command(Command),
    Pipeline(Vec<PipelineStage>),
    Assignment(Assignment),
    If(IfStmt),
    For(ForLoop),
    ToolDef(ToolDef),
    Background(Box<Stmt>),
}

pub struct Command {
    pub name: String,
    pub args: Vec<Arg>,
    pub redirects: Vec<Redirect>,
}

pub enum Arg {
    Positional(Value),
    Named { key: String, value: Value },
}

pub enum Value {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
    VarRef(VarPath),               // ${foo.bar[0]}
    Interpolated(Vec<StringPart>), // "hello ${name}"
}

pub struct ToolDef {
    pub name: String,
    pub params: Vec<ParamDef>,
    pub body: Vec<Stmt>,
}

pub struct ParamDef {
    pub name: String,
    pub typ: ParamType,
    pub default: Option<Value>,
}

// === Runtime ===

pub struct ExecResult {
    pub code: i32,
    pub ok: bool,
    pub err: Option<String>,
    pub out: String,
    pub data: Option<serde_json::Value>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> ToolSchema;
    async fn execute(&self, args: ToolArgs, ctx: &mut ExecContext) -> ExecResult;
}

// === VFS ===

#[async_trait]
pub trait Filesystem: Send + Sync {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    async fn list(&self, path: &Path) -> io::Result<Vec<DirEntry>>;
    async fn stat(&self, path: &Path) -> io::Result<Metadata>;
    async fn mkdir(&self, path: &Path) -> io::Result<()>;
    async fn remove(&self, path: &Path) -> io::Result<()>;
}

pub struct VfsRouter {
    mounts: BTreeMap<PathBuf, Box<dyn Filesystem>>,
}
```

## Shebang Mode

Trivial to implement:

```rust
fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        None => run_repl(),
        Some("serve") => run_server(&args[2..]),
        Some(path) => run_script(path),
    }
}

fn run_script(path: &str) -> Result<()> {
    let source = std::fs::read_to_string(path)?;

    // Skip shebang if present
    let source = if source.starts_with("#!") {
        source.lines().skip(1).collect::<Vec<_>>().join("\n")
    } else {
        source
    };

    let mut shell = Kaish::new();
    shell.execute(&source)
}
```

The shebang (`#!/usr/bin/env kaish`) is handled by the OS — it invokes our binary with the script path. We skip line 1 if it starts with `#!`.

## MCP Server Mode: The Prestige

When running `kaish serve tools.kai`:

1. Parse the script, extract all `tool` definitions
2. Build MCP tool schemas from the `ParamDef`s
3. Start MCP server (stdio or HTTP)
4. On tool call: instantiate a shell, execute the tool body

```rust
async fn serve_script(path: &str) -> Result<()> {
    let source = std::fs::read_to_string(path)?;
    let ast = parse(&source)?;

    // Extract tool definitions
    let tools: Vec<ToolDef> = ast.iter()
        .filter_map(|s| match s {
            Stmt::ToolDef(t) => Some(t.clone()),
            _ => None,
        })
        .collect();

    // Build MCP server
    let server = McpServer::new();
    for tool in tools {
        server.register(ScriptTool::new(tool, source.clone()));
    }

    // Run server
    server.serve_stdio().await
}
```

This means Claude Code can call tools defined in kaish scripts.
**User-defined tools become first-class MCP tools.**

## Parser: logos + chumsky

- **logos** for lexer: fast, derive-macro based
- **chumsky** for parser: beautiful errors, declarative grammar

```rust
// Lexer with logos
#[derive(Logos, Debug, PartialEq)]
pub enum Token {
    #[token("set")]
    Set,
    #[token("tool")]
    Tool,
    #[token("if")]
    If,
    #[token("then")]
    Then,
    // ...
    #[regex(r#""([^"\\]|\\.)*""#)]
    String,
    #[regex(r"[a-zA-Z_][a-zA-Z0-9_-]*")]
    Ident,
    #[regex(r"-?[0-9]+")]
    Int,
    // ...
}

// Parser with chumsky
fn command() -> impl Parser<Token, Command, Error = Simple<Token>> {
    ident()
        .then(arg().repeated())
        .then(redirect().repeated())
        .map(|((name, args), redirects)| Command { name, args, redirects })
}
```

## Async Model

Everything is async (tokio):
- Tool execution is async (MCP calls, file I/O)
- Pipelines spawn tasks for each stage
- Scatter/gather uses `JoinSet` for parallel execution
- Background jobs are spawned tasks with handles

```rust
// Pipeline execution
async fn execute_pipeline(stages: Vec<PipelineStage>, ctx: &mut ExecContext) -> ExecResult {
    let (mut tx, mut rx) = mpsc::channel::<Value>(32);

    let mut handles = Vec::new();
    for (i, stage) in stages.iter().enumerate() {
        let (next_tx, next_rx) = if i < stages.len() - 1 {
            let (tx, rx) = mpsc::channel(32);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        let handle = tokio::spawn(execute_stage(stage, rx, next_tx));
        handles.push(handle);
        rx = next_rx.unwrap_or_else(|| mpsc::channel(1).1);
    }

    // Wait for completion, collect final result
    // ...
}
```

## Integration with Kaijutsu

kaish is designed to be embedded in Kaijutsu (会術):

```rust
// In Kaijutsu
use kaish::Kaish;

let shell = Kaish::builder()
    .mount("/workspace", workspace_vfs)
    .mount("/scratch", MemoryFs::new())
    .mount("/mcp/exa", exa_resources)
    .register_tools(builtin_tools())
    .register_mcp_server("exa", exa_client)
    .build();

// Execute user commands
shell.execute("ls /workspace | grep pattern=rs")?;

// Or run a script
shell.execute_file("automation.kai")?;
```

## Next Steps

1. **Bootstrap**: `cargo new kaish`, set up workspace
2. **Lexer**: Define tokens with logos
3. **Parser**: AST types, basic grammar with chumsky
4. **Minimal interpreter**: Variable assignment, echo, simple pipes
5. **VFS**: Memory backend, mount routing
6. **Builtins**: ls, cat, grep with VFS
7. **MCP integration**: Client wrapper, tool discovery
8. **Scatter/gather**: The 会 magic
9. **Server mode**: MCP export
10. **REPL**: Line editing, history, 会sh> prompt
