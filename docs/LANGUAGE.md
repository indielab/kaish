# kaish (会sh) Language Specification (Draft)

```
会 (kai) = meeting, gathering, coming together
kaish = kai + sh = the gathering shell
        ksh vibes, "ai" in the middle 👀
```

A minimal shell language designed for MCP tool orchestration.
Part of the Kaijutsu (会術) project — the art of gathering.

## Philosophy

- **Everything is a tool** - builtins, MCP tools, same syntax
- **Strings are easy, structure is JSON** - hybrid data model
- **Predictable over powerful** - no dark corners
- **Agent-friendly** - easy to generate, parse, validate
- **Fail fast** - ambiguity is an error, not a guess

## Syntax

### Variables

```bash
set NAME = "value"                  # assignment (set keyword required)
set X = 42                          # numbers
set LIST = ["a", "b", "c"]          # arrays (JSON syntax)
set OBJ = {"key": "value"}          # objects (JSON syntax)

echo ${NAME}                        # interpolation (ONLY this form)
echo "${NAME} more text"            # in strings
echo ${OBJ.key}                     # nested access
echo ${LIST[0]}                     # array index
```

No `$NAME`. No `${NAME:-default}`. No arrays-of-arrays.

### Quoting (JSON Rules)

```bash
echo hello                   # bare word, no quotes needed
echo "hello world"           # double quotes for spaces
echo "line\nbreak"           # JSON escapes work: \n \t \\ \"
echo "value: ${X}"           # interpolation in double quotes
echo "literal \${X}"         # escaped = no interpolation
```

No single quotes. No `$'...'`. No backticks.

### Arguments (JSON only)

```bash
# Named arguments - primitives
tool arg1="value" count=10 enabled=true

# Arrays - JSON syntax (quotes required)
tool items=["one", "two", "three"]

# Objects - JSON syntax (keys quoted)
tool config={"host": "localhost", "port": 8080}

# Nested structures
tool data={"items": [{"id": 1}, {"id": 2}]}

# Positional args work too
echo "hello" "world"
```

**Parser is JSON-only.** REPL provides YAML→JSON expansion via Tab key.

```
会sh> tool config={host: localhost}<TAB>
会sh> tool config={"host": "localhost"}
```

### Pipes & Redirects

```bash
tool-a | tool-b | tool-c     # pipe stdout → stdin
tool > file                  # redirect stdout
tool >> file                 # append stdout
tool < file                  # stdin from file
tool 2> file                 # redirect stderr
tool &> file                 # stdout + stderr
```

No `2>&1`. No process substitution. No here-docs (yet?).

### The Result Type: `$?`

After any command, `$?` contains:

```bash
${?.code}      # int: exit code (0 = success)
${?.ok}        # bool: true if code == 0
${?.err}       # string: error message
${?.out}       # string: raw stdout
${?.data}      # object: parsed JSON (if stdout was JSON)
```

Example:
```bash
api-call endpoint=/users
if ${?.ok}; then
    echo "Got ${?.data.count} users"
else
    echo "Error: ${?.err}"
fi
```

### Control Flow

```bash
# Conditional
if CONDITION; then
    ...
else
    ...
fi

# Loops (minimal)
for ITEM in ${LIST}; do
    process ${ITEM}
done
```

No `case`. No `select`. No arithmetic `(( ))`.

### Command Substitution `$(cmd)`

Run a command and get its structured result as an expression:

```bash
# Check if command succeeded
if $(validate input.json); then
    echo "valid"
fi

# Access specific result fields
if $(validate input.json).ok; then
    echo "validation passed"
fi

# Capture result for later use
set RESULT = $(fetch url="https://api.example.com")
echo ${RESULT.data.items[0].name}

# Logical operators on command results
if $(check-a) && $(check-b); then
    echo "both checks passed"
fi

if $(try-primary) || $(try-fallback); then
    echo "at least one succeeded"
fi
```

The `$(cmd)` expression returns the command's result object:
- `.ok` - bool: true if exit code 0
- `.code` - int: exit code
- `.data` - object: parsed stdout (if JSON)
- `.out` - string: raw stdout
- `.err` - string: error message

**Note**: Unlike bash's `&&`/`||` statement chaining, kaish uses these as expression
operators. This avoids ambiguity around backgrounding (`&`) and redirects.

Nested command substitution is supported:
```bash
set MSG = $(echo $(date))
set RESULT = $(process $(fetch ${URL}))
```

### Background Jobs

```bash
slow-task &                  # run in background
jobs                         # list jobs
fg %1                        # foreground
wait                         # wait for all
wait %1 %2                   # wait for specific
```

### 散・集 (San/Shū) — Scatter/Gather Parallelism

```bash
# 散 (scatter) - fan out to parallel workers
# 集 (gather) - collect results back
cat input.txt | scatter | process-item ${ITEM} | gather > output.json

# With explicit variable
cat input.txt | scatter as=URL limit=4 | fetch url=${URL} | gather

# Options
scatter as=VAR               # bind each item to ${VAR}
scatter limit=N              # max parallelism (default: 8)
gather progress=true         # show progress
gather first=N               # take first N, cancel rest
gather errors=FILE           # collect failures separately
```

## Virtual Filesystem

Paths resolve through VFS abstraction:

```
/bin/                  → available tools (ls /bin/exa)
/src/                  → mounted local paths
/scratch/              → in-memory temp storage
/mcp/<server>/         → MCP server resources
```

Configure mounts:
```bash
mount local:/home/amy/project /src
mount local:/home/amy/project /src-ro readonly=true
mount memory: /scratch
```

Read-only mounts reject all write operations (`>`, `>>`, `write`, `rm`, etc.).

## Tools

Everything is a tool. Builtins:

| Tool | Description |
|------|-------------|
| `echo` | Output text |
| `set` | Set variable |
| `ls` | List directory |
| `cd` | Change directory |
| `pwd` | Print working directory |
| `cat` | Read file |
| `write` | Write to file |
| `mkdir` | Create directory |
| `rm` | Remove file |
| `cp` | Copy |
| `mv` | Move |
| `grep` | Search content |
| `jq` | JSON query |
| `help` | Tool documentation |
| `jobs` | List background jobs |
| `wait` | Wait for jobs |
| `scatter` | 散 — Parallel fan-out |
| `gather` | 集 — Collect parallel results |
| `assert` | Test assertions (error if condition false) |
| `date` | Current timestamp |

MCP tools use same syntax:
```bash
exa.web_search query="rust"
filesystem.read path="/etc/hosts"
```

## What We Explicitly Don't Support

- Single quotes
- `$VAR` (must use `${VAR}`)
- Parameter expansion (`${VAR:-default}`, `${VAR##*/}`, etc.)
- Arithmetic expansion `$(( ))`
- Brace expansion `{a,b,c}`
- Glob expansion `*.txt` (tools handle their own patterns)
- Here-docs `<<EOF`
- Process substitution `<(cmd)`
- Backtick command substitution `` `cmd` `` (use `$(cmd)` instead)
- Statement-level `&&`/`||` chaining (use expression operators instead)
- Aliases
- `eval`
- Arrays of arrays

## User-Defined Tools

Tools can be defined in scripts and **re-exported over MCP**:

```bash
# Define a tool with typed parameters
tool fetch-and-summarize url:string max_length:int=500 {
    fetch url=${url} > /scratch/content
    summarize input=- length=${max_length} < /scratch/content
}

# Use it locally
fetch-and-summarize url="https://example.com"

# Or export the script as an MCP server (see below)
```

Type annotations: `string`, `int`, `float`, `bool`, `array`, `object`
Default values with `=`.

### MCP Export (The Prestige ✨)

Any kaish script can be exposed as an MCP server:

```bash
kaish serve my-tools.kai --port 8080
```

Now Claude Code (or any MCP client) can call:
```
my-tools.fetch-and-summarize url="https://..."
```

This enables:
- **Tool composition** - build complex tools from simple ones
- **User customization** - power users define their own toolkits
- **Agent optimization** - bundle common patterns into single calls

## Execution Modes

### Shebang (Script) Mode
```bash
#!/usr/bin/env kaish
echo "hello from script"
```

Run with: `./script.kai` or `kaish script.kai`

### REPL Mode
```bash
$ kaish
会sh> echo "interactive"
会sh> ls /bin
```

### Embedded Mode
Library API for embedding in other Rust programs (e.g., Kaijutsu):
```rust
let shell = Kaish::new();
shell.mount("/src", LocalFs::new("/home/amy/project"));
shell.register_mcp_server("exa", exa_client);
shell.execute("ls /src | grep pattern=rs")?;
```

### Server Mode (MCP Export)
```bash
kaish serve tools.kai --stdio
```

## Deferred Features

Explicitly not in v0.1, may add later:

- Here-docs `<<EOF`
- Glob expansion in paths
- Process substitution `<(cmd)`
- Object destructuring in scatter (`scatter as={id, url}`)
