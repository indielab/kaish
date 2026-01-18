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
# Both styles work - bash-style preferred
NAME="value"                        # bash-style assignment (no spaces around =)
local NAME = "value"                # explicit local scope (spaces allowed)

X=42                                # numbers
LIST=["a", "b", "c"]                # arrays (JSON syntax)
OBJ={"key": "value"}                # objects (JSON syntax)

# Variable expansion - both forms work
echo $NAME                          # simple expansion (identifiers only)
echo ${NAME}                        # braced expansion (equivalent)
echo "${NAME} more text"            # interpolation in strings
echo ${OBJ.key}                     # nested access (braces required for paths)
echo ${LIST[0]}                     # array index (braces required)
```

Simple `$NAME` works for plain identifiers. Use `${VAR}` for paths: `${OBJ.field}`, `${ARR[0]}`.
No `${NAME:-default}`. No parameter expansion. No arrays-of-arrays.

### Quoting

```bash
echo hello                   # bare word, no quotes needed
echo "hello world"           # double quotes for spaces
echo "line\nbreak"           # JSON escapes work: \n \t \\ \"
echo "value: $X"             # interpolation in double quotes
echo "literal \${X}"         # escaped = no interpolation

# Single quotes - literal strings, no interpolation
echo 'hello $NAME'           # prints: hello $NAME (literal)
echo 'no escapes: \n'        # prints: no escapes: \n (literal backslash-n)
```

No `$'...'`. No backticks.

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

### Exit Code: `$?`

After any command, `$?` is an **integer exit code** (0-255), just like bash:

```bash
some-command
echo $?                      # prints: 0 (or non-zero on failure)

# Use in conditions
if [[ $? == 0 ]]; then
    echo "success"
fi
```

To capture structured output, use command substitution:

```bash
RESULT=$(api-call endpoint=/users)
echo ${RESULT.count}         # if stdout was JSON, access fields
```

### Statement Chaining

Commands can be chained with `&&` and `||` at the statement level:

```bash
# Run cmd2 only if cmd1 succeeds (exit code 0)
cmd1 && cmd2

# Run cmd2 only if cmd1 fails (non-zero exit code)
cmd1 || cmd2

# Chained
mkdir /tmp/work && cd /tmp/work && init-project
try-primary || try-fallback || echo "all failed"
```

### Test Expressions `[[ ]]`

Bash-style test expressions for conditionals:

```bash
# File tests
if [[ -f /path/file ]]; then echo "is file"; fi
if [[ -d /path/dir ]]; then echo "is directory"; fi
if [[ -e /path/any ]]; then echo "exists"; fi

# String tests
if [[ -z $VAR ]]; then echo "empty"; fi
if [[ -n $VAR ]]; then echo "non-empty"; fi

# Comparisons
if [[ $X == "value" ]]; then echo "match"; fi
if [[ $X != "other" ]]; then echo "no match"; fi
if [[ $NUM -gt 5 ]]; then echo "greater"; fi
if [[ $NUM -lt 10 ]]; then echo "less"; fi
```

Note: `[ ]` (single brackets) is reserved for JSON arrays. Use `[[ ]]` for tests.

### Control Flow

```bash
# Conditional
if CONDITION; then
    ...
elif OTHER_CONDITION; then
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

### Comparison Operators

| Operator | Description |
|----------|-------------|
| `==` | Equality |
| `!=` | Inequality |
| `<` | Less than |
| `>` | Greater than |
| `<=` | Less than or equal |
| `>=` | Greater than or equal |
| `=~` | Regex match (returns bool) |
| `!~` | Regex not match (returns bool) |

```bash
# Regex matching
if ${filename} =~ "\.rs$"; then
    echo "Rust source file"
fi

if ${input} !~ "^[0-9]+$"; then
    echo "Not a number"
fi
```

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
| `vars` | List variables (`--json` for JSON output) |
| `tools` | List available tools (`--json` for JSON, `name` for detail) |
| `mounts` | List VFS mount points (`--json` for JSON output) |
| `history` | Show execution history (`--limit=N`, `--json`) |
| `checkpoints` | List checkpoints (`--json` for JSON output) |
| `exec` | Execute external command |

MCP tools use same syntax:
```bash
exa.web_search query="rust"
filesystem.read path="/etc/hosts"
```

## What We Explicitly Don't Support

- Parameter expansion (`${VAR:-default}`, `${VAR##*/}`, etc.)
- Arithmetic expansion `$(( ))`
- Brace expansion `{a,b,c}`
- Glob expansion `*.txt` (tools handle their own patterns)
- Here-docs `<<EOF`
- Process substitution `<(cmd)`
- Backtick command substitution `` `cmd` `` (use `$(cmd)` instead)
- Single bracket tests `[ ]` (reserved for JSON arrays, use `[[ ]]`)
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
