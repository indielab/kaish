# kaish Build Plan — Layered/Bottom-Up

## Dependency Graph

```
L0: Foundation
    └── Cargo workspace, kaish-schema (capnp codegen)

L1: Types & Lexer
    ├── AST types (ast.rs)
    ├── Token definitions (logos)
    └── Value enum

L2: Parser
    └── chumsky grammar → AST

L3: Core Runtime
    ├── Expression evaluator
    ├── Variable scope
    └── ExecResult ($?)

L4: REPL (Evolving) ← NEW: grows with each layer
    ├── kaish-repl crate with rustyline
    ├── Parse → AST display mode (/ast toggle)
    ├── Expression evaluation with Scope
    └── Stub executor for commands (returns dummy $?)

L5: VFS
    ├── Filesystem trait
    ├── MemoryFs (/scratch)
    ├── LocalFs (/src)
    └── VfsRouter (mount points)
    └── REPL+: cat, ls on VFS paths

L6: Tools
    ├── Tool trait
    ├── ToolRegistry
    └── Builtins (echo, set, cat, grep, etc.)
    └── REPL+: real command execution

L7: Pipes & Jobs
    ├── Pipeline execution (tokio channels)
    └── Background jobs (JobManager)
    └── REPL+: pipelines, &, jobs, wait

L8: MCP Client
    ├── McpClient wrapper
    └── Tool discovery
    └── REPL+: MCP tool calls

L9: 散/集 (Scatter/Gather)
    ├── scatter implementation
    └── gather implementation
    └── REPL+: parallel execution

L10: State Persistence
    ├── SQLite integration (rusqlite)
    └── State save/restore
    └── REPL+: session persistence

L11: 核 (Kernel)
    ├── Kernel struct
    ├── Cap'n Proto RPC server
    └── Socket listener

L12: Clients
    ├── EmbeddedClient (wraps Kernel directly)
    └── IpcClient (connects via socket)

L13: Frontends (Final)
    ├── Script runner (kaish script.kai)
    ├── REPL client mode (connects to kernel)
    └── MCP server mode (kaish serve)
```

### REPL Evolution Strategy

The REPL is introduced early (L4) and gains capabilities incrementally:

| Layer | REPL Capabilities |
|-------|-------------------|
| L4 | Parse input, show AST, evaluate expressions, `set` variables |
| L5 | `cat`, `ls` work on VFS paths |
| L6 | All builtins work, real command execution |
| L7 | Pipelines (`a \| b`), background jobs (`&`), `jobs`, `wait` |
| L8 | MCP tool calls (`server.tool arg=val`) |
| L9 | `scatter`/`gather` parallelism |
| L10 | Session save/restore across restarts |
| L13 | Connect to remote kernels via IPC |

This gives us an interactive playground throughout development.

---

## XDG Base Directory Paths

All runtime files follow XDG Base Directory Specification:

| Purpose | XDG Variable | Default | kaish Path |
|---------|--------------|---------|------------|
| Sockets | `$XDG_RUNTIME_DIR` | `/run/user/$UID` | `$XDG_RUNTIME_DIR/kaish/*.sock` |
| State DB | `$XDG_DATA_HOME` | `~/.local/share` | `$XDG_DATA_HOME/kaish/kernels/*.db` |
| Blobs | `$XDG_DATA_HOME` | `~/.local/share` | `$XDG_DATA_HOME/kaish/blobs/` |
| Config | `$XDG_CONFIG_HOME` | `~/.config` | `$XDG_CONFIG_HOME/kaish/config.toml` |
| Cache | `$XDG_CACHE_HOME` | `~/.cache` | `$XDG_CACHE_HOME/kaish/` |

```rust
// paths.rs
use directories::BaseDirs;

pub fn runtime_dir() -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("kaish")
}

pub fn data_dir() -> PathBuf {
    BaseDirs::new()
        .map(|d| d.data_dir().join("kaish"))
        .unwrap_or_else(|| PathBuf::from("~/.local/share/kaish"))
}

pub fn config_dir() -> PathBuf {
    BaseDirs::new()
        .map(|d| d.config_dir().join("kaish"))
        .unwrap_or_else(|| PathBuf::from("~/.config/kaish"))
}
```

### Directory Structure (Runtime)

```
$XDG_RUNTIME_DIR/kaish/
├── default.sock            # Default kernel socket
├── default.pid             # PID for stale detection
├── project-foo.sock        # Named kernel
└── session-abc123.sock     # Ephemeral session

$XDG_DATA_HOME/kaish/
├── kernels/
│   ├── default.db          # Default kernel state
│   └── project-foo.db      # Named kernel state
└── blobs/
    └── <sha256-prefix>/    # Content-addressed blob storage

$XDG_CONFIG_HOME/kaish/
├── config.toml             # Global config
└── tools/                  # User tool definitions
    └── my-tools.kai
```

---

## Layer 0: Foundation

**Goal**: Buildable Cargo workspace with schema generation.

### Files to create:
```
kaish/
├── Cargo.toml              # Workspace root
├── schema/
│   └── kaish.capnp         # ✅ Already exists
└── crates/
    └── kaish-schema/
        ├── Cargo.toml
        ├── build.rs        # capnpc invocation
        └── src/lib.rs      # Re-exports
```

### Dependencies:
```toml
# kaish-schema/Cargo.toml
[dependencies]
capnp = "0.19"

[build-dependencies]
capnpc = "0.19"
```

### Verification:
- `cargo build` succeeds
- Generated types available: `kaish_schema::kaish_capnp::*`

---

## Layer 1: Types & Lexer

**Goal**: Token stream from input string, AST type definitions.

### Files:
```
crates/kaish-kernel/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── lexer/
    │   ├── mod.rs
    │   └── tokens.rs       # logos derive
    └── ast.rs              # AST types (no parser yet)
```

### Key types:
```rust
// tokens.rs
#[derive(Logos, Debug, PartialEq, Clone)]
pub enum Token {
    #[token("set")] Set,
    #[token("tool")] Tool,
    #[token("if")] If,
    #[token("then")] Then,
    #[token("else")] Else,
    #[token("fi")] Fi,
    #[token("for")] For,
    #[token("in")] In,
    #[token("do")] Do,
    #[token("done")] Done,
    #[token("true")] True,
    #[token("false")] False,

    #[token("=")] Eq,
    #[token("|")] Pipe,
    #[token("&")] Amp,
    #[token(">")] Gt,
    #[token(">>")] GtGt,
    #[token("<")] Lt,
    #[token("2>")] Stderr,
    #[token("&>")] Both,
    #[token(";")] Semi,
    #[token(":")] Colon,
    #[token(",")] Comma,
    #[token("{")] LBrace,
    #[token("}")] RBrace,
    #[token("[")] LBracket,
    #[token("]")] RBracket,
    #[token("&&")] And,
    #[token("||")] Or,

    #[regex(r#""([^"\\]|\\.)*""#)]
    String,

    #[regex(r"\$\{[^}]+\}")]
    VarRef,

    #[regex(r"[a-zA-Z_][a-zA-Z0-9_-]*")]
    Ident,

    #[regex(r"-?[0-9]+")]
    Int,

    #[regex(r"-?[0-9]+\.[0-9]+")]
    Float,

    #[regex(r"#[^\n]*")]
    Comment,

    #[regex(r"[ \t]+")]
    Whitespace,

    #[regex(r"\n|\r\n")]
    Newline,
}

// ast.rs
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Assignment(Assignment),
    Command(Command),
    Pipeline(Pipeline),
    If(IfStmt),
    For(ForLoop),
    ToolDef(ToolDef),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub name: String,
    pub args: Vec<Arg>,
    pub redirects: Vec<Redirect>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Positional(Value),
    Named { key: String, value: Value },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
    VarRef(VarPath),
    Interpolated(Vec<StringPart>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct VarPath {
    pub segments: Vec<VarSegment>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VarSegment {
    Field(String),
    Index(usize),
}
```

### Verification:
- Lexer tests: `set X = 5` → `[Set, Ident("X"), Eq, Int(5)]`
- ~50 lexer tests passing

---

## Layer 2: Parser

**Goal**: Parse token stream → AST.

### Files:
```
crates/kaish-kernel/src/
├── parser/
│   ├── mod.rs
│   ├── grammar.rs          # chumsky combinators
│   └── error.rs            # ParseError with spans
```

### Dependencies:
```toml
chumsky = "0.9"
ariadne = "0.4"  # Pretty error reporting
```

### Verification:
- `parse("set X = 5")` → `Ok(Stmt::Assignment(...))`
- `parse("echo ${X}")` → `Ok(Stmt::Command(...))`
- `parse("a | b | c")` → `Ok(Stmt::Pipeline(...))`
- ~100 parser tests passing

---

## Layer 3: Core Runtime

**Goal**: Evaluate expressions, track variables, produce results.

### Files:
```
crates/kaish-kernel/src/
├── interpreter/
│   ├── mod.rs
│   ├── eval.rs             # Expression evaluation
│   ├── scope.rs            # Variable bindings
│   └── result.rs           # ExecResult type
```

### Key behavior:
- `${VAR}` → lookup in scope
- `${VAR.field}` → nested access
- `${?.ok}` → last result access
- String interpolation: `"hello ${NAME}"`

### Verification:
```rust
let mut scope = Scope::new();
scope.set("X", Value::Int(42));
assert_eq!(eval(&parse("${X}"), &scope), Value::Int(42));
```

---

## Layer 4: REPL (Evolving)

**Goal**: Interactive shell that grows with each layer.

### Files:
```
crates/kaish-repl/
├── Cargo.toml
└── src/
    ├── lib.rs              # Core REPL logic
    └── main.rs             # Entry point
```

### Dependencies:
```toml
rustyline = "14"
kaish-kernel = { path = "../kaish-kernel" }
```

### Initial Capabilities (L4):
- Parse input and display AST (`/ast` toggle)
- Evaluate expressions with persistent Scope
- `set X = value` assignments work
- Stub executor returns dummy `$?` for commands
- `/help`, `/quit` meta-commands

### REPL Commands:
```
会sh> set NAME = "Alice"
会sh> echo "Hello ${NAME}"
[stub] echo "Hello Alice"
$? = { ok: true, code: 0, out: "Hello Alice" }

会sh> /ast
AST mode: ON
会sh> set X = 5
Stmt::Assignment { name: "X", value: Literal(Int(5)) }

会sh> /help
Commands: /ast, /scope, /quit, /help
```

### Verification:
- `cargo run -p kaish-repl` launches REPL
- Can set variables and see them in expressions
- `/ast` shows parsed AST
- `/scope` dumps current variables

---

## Layer 5: VFS (formerly L4)

**Goal**: Abstract filesystem with mount points.

### Files:
```
crates/kaish-kernel/src/
├── vfs/
│   ├── mod.rs
│   ├── traits.rs           # Filesystem trait
│   ├── memory.rs           # MemoryFs
│   ├── local.rs            # LocalFs
│   └── router.rs           # VfsRouter
```

### Key trait:
```rust
#[async_trait]
pub trait Filesystem: Send + Sync {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    async fn list(&self, path: &Path) -> io::Result<Vec<DirEntry>>;
    async fn stat(&self, path: &Path) -> io::Result<Metadata>;
    async fn mkdir(&self, path: &Path) -> io::Result<()>;
    async fn remove(&self, path: &Path) -> io::Result<()>;
    fn read_only(&self) -> bool;
}
```

### Verification:
```rust
let mut vfs = VfsRouter::new();
vfs.mount("/scratch", MemoryFs::new());
vfs.mount("/src", LocalFs::new("/home/amy/project").read_only());
vfs.write("/scratch/test.txt", b"hello").await?;
assert!(vfs.write("/src/test.txt", b"fail").await.is_err()); // read-only
```

---

## Layer 5: Tools

**Goal**: Tool trait, registry, builtin implementations.

### Files:
```
crates/kaish-kernel/src/
├── tools/
│   ├── mod.rs
│   ├── traits.rs           # Tool trait
│   ├── registry.rs         # ToolRegistry
│   └── builtin/
│       ├── mod.rs
│       ├── echo.rs
│       ├── set.rs
│       ├── ls.rs
│       ├── cd.rs
│       ├── pwd.rs
│       ├── cat.rs
│       ├── write.rs
│       ├── mkdir.rs
│       ├── rm.rs
│       ├── cp.rs
│       ├── mv.rs
│       ├── grep.rs
│       ├── help.rs
│       ├── jobs.rs
│       ├── wait.rs
│       ├── assert.rs
│       └── date.rs
```

### Key trait:
```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> ToolSchema;
    async fn execute(&self, args: ToolArgs, ctx: &mut ExecContext) -> ExecResult;
}
```

### Verification:
- `echo "hello"` → stdout: "hello"
- `ls /scratch` → list files
- `cat /scratch/test.txt` → file contents

---

## Layer 6: Pipes & Jobs

**Goal**: Pipeline execution, background jobs.

### Files:
```
crates/kaish-kernel/src/
├── scheduler/
│   ├── mod.rs
│   ├── pipeline.rs         # Pipeline execution
│   └── job.rs              # JobManager, JobHandle
```

### Key behavior:
- `a | b | c` → spawn tasks, connect via channels
- `cmd &` → spawn background, return job ID
- `wait` → block on all jobs
- `jobs` → list running

### Verification:
```rust
// Pipeline
execute("echo hello | grep pattern=ell").await;
assert!(last_result.ok);

// Background
execute("slow-task &").await;
assert_eq!(jobs.len(), 1);
wait_all().await;
```

---

## Layer 7: MCP Client

**Goal**: Call MCP tools like builtins.

### Files:
```
crates/kaish-kernel/src/
├── tools/
│   └── mcp.rs              # McpToolWrapper
```

### Behavior:
- `exa.web_search query="rust"` → MCP tool call
- Structured result in `${?.data}`
- Auto-discovery on connect

### Verification:
- Mock MCP server
- Call tool, verify args passed correctly

---

## Layer 8: 散/集 (Scatter/Gather)

**Goal**: Parallel fan-out/fan-in.

### Files:
```
crates/kaish-kernel/src/
├── scheduler/
│   ├── scatter.rs
│   └── gather.rs
```

### Behavior:
```bash
cat items.txt | scatter as=ITEM limit=4 | process ${ITEM} | gather
```
- `scatter`: reads stdin, spawns N parallel pipelines
- `gather`: collects results into JSON array

### Verification:
```rust
// 4 items, limit=2 → 2 concurrent
execute("echo '[1,2,3,4]' | scatter as=N limit=2 | double val=${N} | gather").await;
// Result: [2, 4, 6, 8] (order preserved)
```

---

## Layer 9: State Persistence

**Goal**: Save/restore kernel state to SQLite.

### Files:
```
crates/kaish-kernel/src/
├── state/
│   ├── mod.rs
│   ├── db.rs               # SQLite wrapper
│   └── serialize.rs        # State ↔ SQL
```

### Uses existing: `schema/state.sql`

### State file location:
```
$XDG_DATA_HOME/kaish/kernels/default.db
```

### Verification:
```rust
kernel.set_var("X", Value::Int(42));
kernel.save_state()?;

let kernel2 = Kernel::load_state("default")?;
assert_eq!(kernel2.get_var("X"), Value::Int(42));
```

---

## Layer 10: 核 (Kernel)

**Goal**: Complete kernel with RPC interface.

### Files:
```
crates/kaish-kernel/src/
├── kernel.rs               # Kernel struct, lifecycle
├── rpc.rs                  # Cap'n Proto server impl
└── paths.rs                # XDG path helpers
```

### Socket location:
```
$XDG_RUNTIME_DIR/kaish/default.sock
```

### Verification:
- Kernel boots, accepts commands
- RPC calls work via capnp

---

## Layer 11: Clients

**Goal**: Connect to kernels.

### Files:
```
crates/kaish-client/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── embedded.rs         # Direct in-process
    └── ipc.rs              # Unix socket + capnp
```

### Verification:
```rust
// Embedded
let kernel = Kernel::new();
let client = EmbeddedClient::new(kernel);
client.execute("echo hello").await;

// IPC
let socket = runtime_dir().join("default.sock");
let client = IpcClient::connect(&socket).await?;
client.execute("echo hello").await;
```

---

## Layer 12: Frontends

**Goal**: User-facing entry points.

### Files:
```
crates/kaish-repl/          # REPL frontend
crates/kaish-mcp/           # MCP server frontend
src/main.rs                 # CLI binary
```

### Verification:
- `kaish script.kai` → runs script
- `kaish` → REPL with 会sh> prompt
- `kaish serve tools.kai --stdio` → MCP server

---

## Build Order Summary

| Layer | Crate | Est. Tests | Checkpoint |
|-------|-------|------------|------------|
| L0 | kaish-schema | 0 | `cargo build` works |
| L1 | kaish-kernel (lexer) | 50 | Token stream correct |
| L2 | kaish-kernel (parser) | 100 | AST from source |
| L3 | kaish-kernel (runtime) | 80 | Variables, $? work |
| **L4** | **kaish-repl** | **20** | **Interactive REPL works** |
| L5 | kaish-kernel (vfs) | 60 | Mount, read, write |
| L6 | kaish-kernel (tools) | 100 | Builtins work |
| L7 | kaish-kernel (pipes) | 40 | Pipelines, jobs |
| L8 | kaish-kernel (mcp) | 30 | MCP calls work |
| L9 | kaish-kernel (scatter) | 50 | 散/集 parallelism |
| L10 | kaish-kernel (state) | 40 | Save/restore |
| L11 | kaish-kernel (kernel) | 30 | Full kernel |
| L12 | kaish-client | 20 | Both client types |
| L13 | frontends (final) | 30 | Script runner, MCP serve |
| **Total** | | **~650** | |

---

## First PR: L0-L2 (Foundation → Parser)

Deliverable: Parse kaish source → AST, with tests.

```bash
cargo test -p kaish-schema
cargo test -p kaish-kernel --lib lexer
cargo test -p kaish-kernel --lib parser
```

This gives us something tangible to iterate on before runtime complexity.

---

## Dependencies (Cargo.toml)

```toml
# Workspace root
[workspace]
resolver = "2"
members = [
    "crates/kaish-schema",
    "crates/kaish-kernel",
    "crates/kaish-client",
    "crates/kaish-repl",
    "crates/kaish-mcp",
]

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
thiserror = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"
tracing-subscriber = "0.3"

# L0
capnp = "0.19"
capnpc = "0.19"

# L1-L2
logos = "0.14"
chumsky = "0.9"
ariadne = "0.4"

# L4
# (no extra deps, just async-trait)

# L9
rusqlite = { version = "0.31", features = ["bundled"] }

# L11
capnp-rpc = "0.19"

# L12
rustyline = "14"
directories = "5"

# Testing
proptest = "1"
```
