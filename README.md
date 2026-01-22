# kaish (会sh)

```
会 (kai) = meeting, gathering, coming together
kaish = kai + sh = the gathering shell
        ksh vibes, "ai" in the middle 👀
```

A Bourne-lite shell for MCP tool orchestration. Part of the [Kaijutsu](https://github.com/tobert/kaijutsu) (会術) project — the art of gathering.

## Philosophy

**80% of a POSIX shell, 100% unambiguous.**

- **Bourne-lite** — familiar syntax, no surprises
- **Everything is a tool** — builtins and MCP tools use identical syntax
- **Predictable over powerful** — if bash has a confusing edge case, kaish doesn't have that feature
- **ShellCheck-clean** — the Bourne subset passes `shellcheck --enable=all`
- **Agent-friendly** — easy to generate, parse, validate
- **Fail fast** — ambiguity is an error, not a guess

## Quick Tour

```bash
#!/usr/bin/env kaish

# Variables - bash style
GREETING="Hello"
CONFIG='{"host": "localhost", "port": 8080}'

# Both $VAR and ${VAR} work
echo "$GREETING, world! 🦀"
echo "Host: ${CONFIG}"

# Control flow
if [[ -f config.json ]]; then
    echo "Config found"
elif [[ -d /etc/kaish ]]; then
    echo "System config exists"
else
    echo "No config"
fi

# Loops
for item in $ITEMS; do
    process $item
done

while [[ $RETRIES -gt 0 ]]; do
    try-operation && break
    RETRIES=$((RETRIES - 1))
done

# Parameter expansion
NAME=${NAME:-"default"}      # default value
echo "Length: ${#NAME}"      # string length

# MCP tools look like builtins
exa.web_search query="rust parser combinators"

# 散/集 (san/shū) — scatter/gather parallelism
cat urls.txt | scatter as=URL limit=4 | fetch url=$URL | gather > results.json

# User-defined tools can be exported as MCP servers
tool summarize url:string max_words:int=200 {
    fetch url=$url > /scratch/content
    llm.summarize input=- words=$max_words < /scratch/content
}

# Export this script as an MCP server
# $ kaish serve my-tools.kai --stdio
```

## What Works

| Feature | Status | Notes |
|---------|--------|-------|
| Variables | ✅ | `VAR=value`, `local VAR=value` |
| Expansion | ✅ | `$VAR`, `${VAR}`, `${?.field}` (exit status fields) |
| Parameter expansion | ✅ | `${VAR:-default}`, `${#VAR}` |
| Single quotes | ✅ | Literal strings, no interpolation |
| Double quotes | ✅ | Interpolation with `$VAR` |
| Test expressions | ✅ | `[[ -f file ]]`, `[[ $X == "y" ]]` |
| Control flow | ✅ | `if/elif/else/fi`, `for/do/done`, `while/do/done` |
| Control statements | ✅ | `break`, `continue`, `return`, `exit` |
| Chaining | ✅ | `&&`, `||` |
| Positional params | ✅ | `$0`-`$9`, `$@`, `$#` |
| Flags | ✅ | `-l`, `--force`, `--message="x"` |
| Pipes & redirects | ✅ | `|`, `>`, `>>`, `<`, `2>`, `&>` |
| Background jobs | ✅ | `&`, `jobs`, `wait`, `fg` |
| Script sourcing | ✅ | `source file.kai`, `. file.kai` |
| Error mode | ✅ | `set -e` (exit on error) |
| Scatter/gather | ✅ | `散/集` parallelism |
| MCP integration | ✅ | Call MCP tools, export scripts as servers |

## What's Intentionally Missing

These bash features are omitted because they're confusing, error-prone, or ambiguous:

- Arithmetic `$(( ))` — use tools for math (SC2004)
- Brace expansion `{a,b,c}` — just write it out (SC1083)
- Glob expansion `*.txt` — tools handle their own patterns (SC2035)
- Here-docs `<<EOF` — use files or strings
- Process substitution `<(cmd)` — use temp files
- Backtick substitution `` `cmd` `` — use `$(cmd)` (SC2006)
- Single bracket tests `[ ]` — use `[[ ]]` (SC2039)
- Aliases, `eval` — explicit is better
- Complex data types — JSON strings + `jq` instead

## Beyond Bourne

Kaish extends Bourne shell with features designed for modern tool orchestration.

**Design principle:** If ShellCheck would warn about it in bash, kaish doesn't have that feature. This eliminates entire classes of bugs:
- No word splitting → SC2086, SC2046 warnings impossible
- No glob expansion → SC2035, SC2144 warnings impossible
- No backticks → SC2006 warnings impossible

See [docs/SHELLCHECK.md](docs/SHELLCHECK.md) for the full mapping.

| Feature | POSIX/Bourne | Kaish | Rationale |
|---------|--------------|-------|-----------|
| **Floats** | ❌ Integer only | ✅ Native `3.14` | MCP tools return JSON with floats |
| **Booleans** | ❌ Exit codes | ✅ Native `true`/`false` | JSON interop, clearer conditions |
| **JSON strings** | ❌ | ✅ `'{"key": "value"}'` | Store JSON, process with `jq` |
| **Typed params** | ❌ | ✅ `name:string` | Tool definitions with validation |
| **Scatter/gather** | ❌ | ✅ `散/集` | Built-in parallelism |
| **VFS** | ❌ | ✅ `/mcp/`, `/scratch/` | Unified resource access |
| **Ambiguity errors** | ❌ Guesses | ✅ Rejects `TRUE`, `yes`, `123abc` | Agent-friendly, fail-fast |

**For AI agents**: Kaish validates inputs strictly. `TRUE` and `yes` are errors (use `true`), `123abc` is rejected, `.5` requires `0.5`. This prevents common generation mistakes from silently succeeding.

## 散・集 (San/Shū) — Scatter/Gather

Fan-out parallelism made easy:

```bash
# 散 (scatter) - fan out to parallel workers
# 集 (gather) - collect results back
cat items.txt | scatter as=ITEM limit=8 | process $ITEM | gather > results.json

# With progress and error handling
cat big_list.txt \
    | scatter as=ID limit=4 \
    | risky-operation id=$ID \
    | gather progress=true errors=/scratch/failed.json
```

## Virtual Filesystem

Paths resolve through VFS abstraction:

```
/bin/              → available tools (ls /bin/exa)
/src/              → mounted local paths
/scratch/          → in-memory temp storage
/mcp/<server>/     → MCP server resources
```

```bash
mount local:/home/amy/project /src
mount local:/home/amy/project /src-ro readonly=true
mount memory: /scratch
```

## MCP Export (The Prestige ✨)

Any kaish script can be exposed as an MCP server:

```bash
$ kaish serve my-tools.kai --stdio
```

Now Claude Code (or any MCP client) can call your user-defined tools directly.

## Builtin Tools

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
| `assert` | Test assertions |
| `date` | Current timestamp |
| `vars` | List variables |
| `tools` | List available tools |
| `mounts` | List VFS mounts |
| `history` | Show execution history |
| `checkpoints` | List checkpoints |

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                          Frontends                                  │
│  ┌─────────┐  ┌─────────┐  ┌─────────┐  ┌─────────────────────────┐ │
│  │  REPL   │  │ Script  │  │   MCP   │  │     Kaijutsu /          │ │
│  │         │  │ Runner  │  │ Server  │  │     Embedded            │ │
│  └────┬────┘  └────┬────┘  └────┬────┘  └───────────┬─────────────┘ │
└───────┼────────────┼────────────┼───────────────────┼───────────────┘
        │            │            │                   │
        └────────────┴─────┬──────┴───────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────────────────┐
│                    会sh 核 (Kaku) — Kernel                          │
│  ┌────────────────────────────────────────────────────────────────┐ │
│  │ State: variables, tool definitions, VFS mounts, job handles   │ │
│  └────────────────────────────────────────────────────────────────┘ │
│                                                                     │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────────┐   │
│  │    Lexer     │  │    Parser    │  │       Interpreter        │   │
│  │   (logos)    │  │   (chumsky)  │  │   (async, tokio-based)   │   │
│  └──────────────┘  └──────────────┘  └──────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────┘
```

The 核 (kaku/kernel) is the unit of execution. Frontends (REPL, script runner, MCP server) connect via:
- **Embedded** — direct in-process (for Kaijutsu)
- **IPC** — Unix sockets with Cap'n Proto RPC

State is persisted in SQLite (WAL mode) for crash recovery and incremental updates.

## Status

**Implementation complete through L14.** All layers from the build plan are implemented.

## Documentation

- [Language Specification](docs/LANGUAGE.md) — syntax, semantics, examples
- [Formal Grammar](docs/GRAMMAR.md) — EBNF, ambiguity analysis, test categories
- [ShellCheck Alignment](docs/SHELLCHECK.md) — SC code mapping, design rationale
- [Architecture](docs/ARCHITECTURE.md) — 核 design, crate structure, protocols
- [Build Plan](docs/BUILD.md) — 14-layer bottom-up implementation
- [Testing Strategy](docs/TESTING.md) — 10:1 test-to-feature ratio target
- [漢字 Reference](docs/kanji.md) — kanji vocabulary for the project
- [Examples](examples/) — annotated scripts

## Schema

- [`schema/kaish.capnp`](schema/kaish.capnp) — Cap'n Proto schema (kernel protocol, types)
- [`schema/state.sql`](schema/state.sql) — SQLite schema (kernel state persistence)

## License

MIT

---

*"The gathering shell" — because orchestrating AI tools should feel like conducting a symphony, not wrestling with syntax.*
