# kaish (会sh)

```
会 (kai) = meeting, gathering, coming together
kaish = kai + sh = the gathering shell
        ksh vibes, "ai" in the middle 👀
```

A minimal shell language for MCP tool orchestration. Part of the [Kaijutsu](https://github.com/tobert/kaijutsu) (会術) project — the art of gathering.

## Philosophy

- **Everything is a tool** — builtins and MCP tools use identical syntax
- **Strings are easy, structure is JSON** — no YAML-lite ambiguity
- **Predictable over powerful** — no dark corners
- **Agent-friendly** — easy to generate, parse, validate
- **Fail fast** — ambiguity is an error, not a guess

## Quick Tour

```bash
#!/usr/bin/env kaish

# Variables with explicit 'set' keyword
set GREETING = "Hello"
set CONFIG = {"host": "localhost", "port": 8080}

# Interpolation only via ${VAR} (no $VAR!)
echo "${GREETING}, world! 🦀"
echo "Host: ${CONFIG.host}"

# MCP tools look like builtins
exa.web_search query="rust parser combinators"
echo "First result: ${?.data.results[0].title}"

# 散/集 (san/shū) — scatter/gather parallelism
cat urls.txt | scatter as=URL limit=4 | fetch url=${URL} | gather > results.json

# User-defined tools can be exported as MCP servers
tool summarize url:string max_words:int=200 {
    fetch url=${url} > /scratch/content
    llm.summarize input=- words=${max_words} < /scratch/content
}

# Export this script as an MCP server
# $ kaish serve my-tools.kai --stdio
```

## Features

### JSON-Only Syntax

The parser accepts strict JSON for structured data. The REPL provides Tab expansion for convenience:

```
会sh> cmd config={host: localhost}<TAB>
会sh> cmd config={"host": "localhost"}
```

### Structured Results (`$?`)

Every command populates a structured result:

```bash
api-call endpoint=/users
if ${?.ok}; then
    echo "Got ${?.data.count} users"
else
    echo "Error: ${?.err}"
fi
```

### 散・集 (San/Shū) — Scatter/Gather

Fan-out parallelism made easy:

```bash
# 散 (scatter) - fan out to parallel workers
# 集 (gather) - collect results back
cat items.txt | scatter as=ITEM limit=8 | process ${ITEM} | gather > results.json

# With progress and error handling
cat big_list.txt \
    | scatter as=ID limit=4 \
    | risky-operation id=${ID} \
    | gather progress=true errors=/scratch/failed.json
```

### Virtual Filesystem

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

### MCP Export (The Prestige ✨)

Any kaish script can be exposed as an MCP server:

```bash
$ kaish serve my-tools.kai --stdio
```

Now Claude Code (or any MCP client) can call your user-defined tools directly.

## Builtin Tools

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
| `assert` | Test assertions |
| `date` | Current timestamp |

## What We Explicitly Don't Support

- Single quotes
- `$VAR` (must use `${VAR}`)
- Parameter expansion (`${VAR:-default}`, `${VAR##*/}`, etc.)
- Arithmetic expansion `$(( ))`
- Brace expansion `{a,b,c}`
- Glob expansion `*.txt` (tools handle their own patterns)
- Here-docs `<<EOF`
- Process substitution `<(cmd)`
- Aliases
- `eval`
- Arrays of arrays

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

The 核 (kaku/kernel) is the unit of execution. Frontends (REPL, script runner, MCP server) connect to kernels via:
- **Embedded** — direct in-process (for Kaijutsu)
- **IPC** — Unix sockets with Cap'n Proto RPC

State is persisted in SQLite (WAL mode) for crash recovery and incremental updates.

## Status

**Design phase.** Documentation is ahead of implementation.

## Documentation

- [Language Specification](docs/LANGUAGE.md) — syntax, semantics, examples
- [Formal Grammar](docs/GRAMMAR.md) — EBNF, ambiguity analysis, test categories
- [Architecture](docs/ARCHITECTURE.md) — 核 design, crate structure, protocols
- [Build Plan](docs/BUILD.md) — 12-layer bottom-up implementation
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
