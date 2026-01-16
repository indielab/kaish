@0xb78d9e8a7c6f5e4d;  # Unique file ID

# kaish (会sh) Cap'n Proto Schema
# Kernel protocol + state serialization

# ============================================================
# Core Value Types
# ============================================================

struct Value {
  union {
    null @0 :Void;
    bool @1 :Bool;
    int @2 :Int64;
    float @3 :Float64;
    string @4 :Text;
    array @5 :List(Value);
    object @6 :List(KeyValue);
    blob @7 :BlobRef;              # Large binary data (streamed)
  }
}

# Reference to a blob stored elsewhere (SQLite, filesystem, etc.)
struct BlobRef {
  id @0 :Text;                     # Unique identifier
  size @1 :UInt64;                 # Size in bytes
  contentType @2 :Text;            # MIME type hint
  hash @3 :Data;                   # SHA-256 for integrity (optional)
}

# For streaming large blobs over RPC
interface BlobStream {
  # Read next chunk (returns empty when done)
  read @0 (maxBytes :UInt32) -> (data :Data, done :Bool);

  # Get total size if known
  size @1 () -> (bytes :UInt64, known :Bool);

  # Cancel streaming
  cancel @2 () -> ();
}

struct KeyValue {
  key @0 :Text;
  value @1 :Value;
}

# ============================================================
# AST Types (for tool definitions)
# ============================================================

struct ToolDef {
  name @0 :Text;
  params @1 :List(ParamDef);
  body @2 :Text;  # Source code, re-parsed on load
  # Could also store AST, but source is more flexible
}

struct ParamDef {
  name @0 :Text;
  type @1 :ParamType;
  default @2 :Value;  # null if required
}

enum ParamType {
  string @0;
  int @1;
  float @2;
  bool @3;
  array @4;
  object @5;
}

# ============================================================
# Execution Results
# ============================================================

struct ExecResult {
  code @0 :Int32;
  ok @1 :Bool;
  err @2 :Text;       # Empty if ok
  stdout @3 :Data;    # Raw bytes
  stderr @4 :Data;    # Raw bytes
  data @5 :Value;     # Parsed JSON if applicable
}

# ============================================================
# VFS Configuration
# ============================================================

struct MountConfig {
  path @0 :Text;           # Mount point, e.g., "/src"
  backend @1 :MountBackend;
  readOnly @2 :Bool;       # Reject write operations
}

struct MountBackend {
  union {
    memory @0 :Void;                    # In-memory scratch
    local @1 :LocalFsConfig;            # Local filesystem
    mcp @2 :McpResourceConfig;          # MCP server resources
  }
}

struct LocalFsConfig {
  rootPath @0 :Text;       # Actual filesystem path
}

struct McpResourceConfig {
  serverName @0 :Text;     # MCP server name
  resourcePrefix @1 :Text; # Resource URI prefix
}

# ============================================================
# MCP Server Configuration
# ============================================================

struct McpServerConfig {
  name @0 :Text;           # Local name, e.g., "exa"
  transport @1 :McpTransport;
}

struct McpTransport {
  union {
    stdio @0 :StdioTransport;
    http @1 :HttpTransport;
    sse @2 :SseTransport;
  }
}

struct StdioTransport {
  command @0 :Text;
  args @1 :List(Text);
  env @2 :List(KeyValue);
}

struct HttpTransport {
  baseUrl @0 :Text;
  headers @1 :List(KeyValue);
}

struct SseTransport {
  url @0 :Text;
  headers @1 :List(KeyValue);
}

# ============================================================
# Kernel State (for serialization)
# ============================================================

struct KernelState {
  version @0 :UInt32;              # Schema version for migration
  timestamp @1 :Int64;             # Unix timestamp of snapshot

  variables @2 :List(KeyValue);    # Variable bindings
  tools @3 :List(ToolDef);         # User-defined tools
  mounts @4 :List(MountConfig);    # VFS configuration
  mcpServers @5 :List(McpServerConfig);  # MCP connections

  lastResult @6 :ExecResult;       # $? state
  cwd @7 :Text;                    # Current working directory

  # Optional metadata
  sessionId @8 :Text;
  metadata @9 :List(KeyValue);     # Extensible
}

# ============================================================
# Kernel Protocol (RPC Interface)
# ============================================================

interface Kernel {
  # --- Execution ---
  execute @0 (input :Text) -> (result :ExecResult);
  executeStreaming @1 (input :Text) -> (stream :OutputStream);

  # --- Variables ---
  getVar @2 (name :Text) -> (value :Value);
  setVar @3 (name :Text, value :Value) -> ();
  listVars @4 () -> (vars :List(KeyValue));

  # --- Tools ---
  listTools @5 () -> (tools :List(ToolInfo));
  getToolSchema @6 (name :Text) -> (schema :ToolSchema);
  callTool @7 (name :Text, args :List(KeyValue)) -> (result :ExecResult);

  # --- Jobs ---
  listJobs @8 () -> (jobs :List(JobInfo));
  cancelJob @9 (id :UInt64) -> (success :Bool);
  waitJob @10 (id :UInt64) -> (result :ExecResult);

  # --- VFS ---
  mount @11 (config :MountConfig) -> ();
  unmount @12 (path :Text) -> ();
  listMounts @13 () -> (mounts :List(MountConfig));

  # --- MCP ---
  registerMcp @14 (config :McpServerConfig) -> ();
  unregisterMcp @15 (name :Text) -> ();
  listMcpServers @16 () -> (servers :List(McpServerConfig));

  # --- State ---
  snapshot @17 () -> (state :KernelState);
  restore @18 (state :KernelState) -> ();
  reset @19 () -> ();  # Clear all state

  # --- Blobs ---
  readBlob @20 (id :Text) -> (stream :BlobStream);
  writeBlob @21 (contentType :Text, size :UInt64) -> (id :Text, stream :BlobSink);
  deleteBlob @22 (id :Text) -> (success :Bool);

  # --- Lifecycle ---
  ping @23 () -> (pong :Text);
  shutdown @24 () -> ();
}

# For writing blobs
interface BlobSink {
  write @0 (data :Data) -> ();
  finish @1 () -> (hash :Data);  # Returns SHA-256
  abort @2 () -> ();
}

# Output streaming for long-running commands
interface OutputStream {
  read @0 () -> (chunk :OutputChunk);
  cancel @1 () -> ();
}

struct OutputChunk {
  union {
    stdout @0 :Data;
    stderr @1 :Data;
    done @2 :ExecResult;
    error @3 :Text;
  }
}

# ============================================================
# Tool Metadata
# ============================================================

struct ToolInfo {
  name @0 :Text;
  description @1 :Text;
  source @2 :ToolSource;
}

enum ToolSource {
  builtin @0;
  user @1;
  mcp @2;
}

struct ToolSchema {
  name @0 :Text;
  description @1 :Text;
  params @2 :List(ParamDef);
  returns @3 :Text;  # Return type description
}

# ============================================================
# Job Info
# ============================================================

struct JobInfo {
  id @0 :UInt64;
  command @1 :Text;
  status @2 :JobStatus;
  startTime @3 :Int64;
  pid @4 :Int32;  # If applicable
}

enum JobStatus {
  running @0;
  completed @1;
  failed @2;
  cancelled @3;
}
