# kaish (会sh) Language Specification

```
会 (kai) = meeting, gathering, coming together
kaish = kai + sh = the gathering shell
        ksh vibes, "ai" in the middle 👀
```

A Bourne-lite shell designed for MCP tool orchestration.
Part of the Kaijutsu (会術) project — the art of gathering.

## Philosophy

**80% of a POSIX shell, 100% unambiguous.**

- **Bourne-lite** — familiar syntax, no surprises
- **Everything is a tool** — builtins and MCP tools use identical syntax
- **Predictable over powerful** — no dark corners
- **ShellCheck-clean** — the Bourne subset passes `shellcheck --enable=all`
- **Agent-friendly** — easy to generate, parse, validate
- **Fail fast** — ambiguity is an error, not a guess

### ShellCheck Alignment

Kaish's Bourne subset is designed to pass `shellcheck --enable=all`. Features
that ShellCheck warns about (word splitting, glob expansion, backticks) are
not implemented. This means code that parses in kaish won't trigger ShellCheck
warnings when the Bourne-compatible subset is extracted.

See [SHELLCHECK.md](SHELLCHECK.md) for detailed rule mapping.

## Syntax

### Variables

```bash
# Assignment - bash style (no spaces around =)
NAME="value"
X=42
ITEMS="one two three"

# Local scope (explicit)
local NAME="value"

# Both $VAR and ${VAR} work
echo $NAME                          # simple expansion
echo ${NAME}                        # braced expansion (equivalent)
echo "${NAME} more text"            # interpolation in strings
```

### Data Types

Kaish has simple, JSON-compatible data types:

```bash
# Strings
NAME="hello"
PATH='/literal/path'
ITEMS="one two three"               # space-separated for iteration

# Integers
COUNT=42
OFFSET=-10

# Floats (⚠️ EXTENSION: POSIX shell has integers only)
PI=3.14159
RATE=-0.5
ZERO=0.0                            # must have digits on both sides of decimal

# Booleans (⚠️ EXTENSION: must be lowercase)
ENABLED=true
DEBUG=false
# TRUE, True, yes, Yes, no, No → ERROR (ambiguous)

# Null
EMPTY=null

# JSON (stored as strings, processed with jq)
DATA='{"name": "alice", "count": 42}'
NAME=$(echo $DATA | jq -r '.name')
```

**Why not arrays/objects?** MCP tools return JSON which is stored as strings and processed with `jq`. This follows the familiar `curl | jq` pattern. For complex data manipulation, use Rhai scripts.

**Why floats?** MCP tools return JSON, which has floats. Rather than force string conversion everywhere, kaish supports them natively. The lexer validates strictly: `.5` and `5.` are errors (use `0.5` and `5.0`).

**Why strict booleans?** AI agents sometimes generate `True`, `YES`, or `1` for booleans. Kaish rejects these ambiguous forms to catch mistakes early rather than silently misinterpreting intent.

### Parameter Expansion

```bash
# Default values
NAME=${NAME:-"default"}             # use "default" if NAME unset or empty

# String length
echo ${#NAME}                       # prints length of NAME

# Positional parameters (in scripts/tools)
echo $0                             # script/tool name
echo $1 $2 $3                       # first three args
echo $@                             # all args
echo $#                             # arg count
```

### Quoting

```bash
# Double quotes - interpolation works
echo "hello world"
echo "value: $X"
echo "path: ${HOME}/file"
echo "line\nbreak"                  # escapes work: \n \t \\ \"
echo "literal \$X"                  # escaped = no interpolation

# Single quotes - literal strings, no interpolation
echo 'hello $NAME'                  # prints: hello $NAME (literal)
echo 'no escapes: \n'               # prints: no escapes: \n
```

No `$'...'`. No backticks.

### Arguments

```bash
# Positional args
echo "hello" "world"

# Named arguments (key=value, no spaces)
tool arg1="value" count=10 enabled=true

# Flag arguments
ls -l                               # short flag
ls -la                              # combined short flags
git commit -m "message"             # short flag with value
git push --force                    # long flag
curl --header="Content-Type: json"  # long flag with value

# Flags with values
cmd -o outfile                      # -o takes next arg as value
cmd --output=outfile                # long form with =
```

### Pipes & Redirects

```bash
tool-a | tool-b | tool-c            # pipe stdout → stdin
tool > file                         # redirect stdout
tool >> file                        # append stdout
tool < file                         # stdin from file
tool 2> file                        # redirect stderr
tool &> file                        # stdout + stderr
```

No `2>&1`. No process substitution. No here-docs.

### Exit Code: `$?`

After any command, `$?` is an **integer exit code** (0-255), just like bash:

```bash
some-command
echo $?                             # prints: 0 (or non-zero on failure)

# Use in conditions
if [[ $? -eq 0 ]]; then
    echo "success"
fi
```

### Statement Chaining

Commands can be chained with `&&` and `||`:

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
if [[ -r /path/file ]]; then echo "readable"; fi
if [[ -w /path/file ]]; then echo "writable"; fi
if [[ -x /path/file ]]; then echo "executable"; fi

# String tests
if [[ -z $VAR ]]; then echo "empty"; fi
if [[ -n $VAR ]]; then echo "non-empty"; fi

# Comparisons
if [[ $X == "value" ]]; then echo "match"; fi
if [[ $X != "other" ]]; then echo "no match"; fi
if [[ $NUM -gt 5 ]]; then echo "greater"; fi
if [[ $NUM -lt 10 ]]; then echo "less"; fi
if [[ $NUM -ge 5 ]]; then echo "greater or equal"; fi
if [[ $NUM -le 10 ]]; then echo "less or equal"; fi

# Regex matching
if [[ $filename =~ "\.rs$" ]]; then echo "Rust file"; fi
if [[ $input !~ "^[0-9]+$" ]]; then echo "not a number"; fi
```

Note: `[ ]` (single brackets) is not supported — use `[[ ]]` for all tests.

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

# For loop (POSIX word-splitting)
for ITEM in "one two three"; do      # iterates over: one, two, three
    process $ITEM
done

# Or with variables
ITEMS="alpha beta gamma"
for ITEM in $ITEMS; do
    echo $ITEM
done

# While loop
while CONDITION; do
    ...
done

# Case statement (pattern matching)
case $VAR in
    hello) echo "matched hello" ;;
    "*.rs") echo "Rust file" ;;    # glob patterns in quotes
    "y"|"yes") echo "yes" ;;       # multiple patterns
    "*") echo "default" ;;         # wildcard default
esac

# Control statements
break                               # exit innermost loop
break 2                             # exit 2 levels of loops
continue                            # skip to next iteration
continue 2                          # skip in outer loop
return                              # return from tool (exit code 0)
return 1                            # return with specific exit code
exit                                # exit script (exit code 0)
exit 1                              # exit with specific code
```

No `select`. No arithmetic `(( ))`.

### Comparison Operators

| Operator | Description |
|----------|-------------|
| `==` | Equality |
| `!=` | Inequality |
| `-lt` | Less than (numeric) |
| `-gt` | Greater than (numeric) |
| `-le` | Less than or equal (numeric) |
| `-ge` | Greater than or equal (numeric) |
| `=~` | Regex match |
| `!~` | Regex not match |

### Command Substitution `$(cmd)`

Run a command and capture output:

```bash
# Capture output
NOW=$(date)
echo "Current time: $NOW"

# Use in conditions
if $(validate input.json); then
    echo "valid"
fi

# Nested
RESULT=$(process $(fetch $URL))
```

### Error Handling

```bash
# Exit on error mode
set -e                              # script exits on first error

# Within script - check and handle
some-command || {
    echo "Command failed"
    exit 1
}

# Source other scripts
source utils.kai                    # load utilities
. config.kai                        # dot notation also works
```

### Background Jobs

```bash
slow-task &                         # run in background
jobs                                # list jobs
fg %1                               # foreground
wait                                # wait for all
wait %1 %2                          # wait for specific
```

### 散・集 (San/Shū) — Scatter/Gather Parallelism

```bash
# 散 (scatter) - fan out to parallel workers
# 集 (gather) - collect results back
cat input.txt | scatter | process-item $ITEM | gather > output.json

# With explicit variable
cat input.txt | scatter as=URL limit=4 | fetch url=$URL | gather

# Options
scatter as=VAR                      # bind each item to $VAR
scatter limit=N                     # max parallelism (default: 8)
gather progress=true                # show progress
gather first=N                      # take first N, cancel rest
gather errors=FILE                  # collect failures separately
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
| `exec` | Execute external command |
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

MCP tools use same syntax:
```bash
exa.web_search query="rust"
filesystem.read path="/etc/hosts"
```

## What's Intentionally Missing

These bash features are omitted because they're confusing, error-prone, or ambiguous:

| Feature | Reason | ShellCheck |
|---------|--------|------------|
| Arithmetic `$(( ))` | Use tools for math | SC2004 |
| Brace expansion `{a,b,c}` | Just write it out | SC1083 |
| Glob expansion `*.txt` | Tools handle their own patterns | SC2035 |
| Here-docs `<<EOF` | Use files or strings | — |
| Process substitution `<(cmd)` | Use temp files | — |
| Backtick substitution `` `cmd` `` | Use `$(cmd)` | SC2006 |
| Single bracket tests `[ ]` | Use `[[ ]]` | SC2039 |
| Aliases | Explicit is better | — |
| `eval` | Security and predictability | SC2091 |
| Arrays of arrays | Keep it simple | — |
| `2>&1` fd duplication | Just use `&>` for combined output | SC2069 |

## User-Defined Tools

Tools can be defined in scripts and **re-exported over MCP**. The `function` keyword can be used as an alias for `tool` (for bash users):

```bash
# Define a tool with typed parameters
tool fetch-and-summarize url:string max_length:int=500 {
    fetch url=$url > /scratch/content
    summarize input=- length=$max_length < /scratch/content
}

# Or use 'function' keyword (bash-compatible alias)
function greet name:string {
    echo "Hello, ${name}!"
}

# Use it locally
fetch-and-summarize url="https://example.com"

# Or export the script as an MCP server (see below)
```

Type annotations: `string`, `int`, `float`, `bool`
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
- **Tool composition** — build complex tools from simple ones
- **User customization** — power users define their own toolkits
- **Agent optimization** — bundle common patterns into single calls

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
- Arithmetic expansion `$(( ))`
- `case` statements
