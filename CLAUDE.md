# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**kaish** (会sh — "the gathering shell") is a Bourne-lite shell for MCP tool orchestration. Part of the [Kaijutsu](https://github.com/tobert/kaijutsu) project.

**Status**: Implementation complete through L14. All build layers are implemented.

## Philosophy

**80% of a POSIX shell, 100% unambiguous.**

- **Bourne-lite** — familiar syntax, no dark corners
- **Everything is a tool** — builtins and MCP tools use identical syntax
- **Predictable over powerful** — if bash has a confusing edge case, kaish doesn't have that feature
- **Agent-friendly** — easy to generate, parse, validate
- **Fail fast** — ambiguity is an error, not a guess

## Build Commands

```bash
cargo build                              # Build workspace
cargo build -p kaish-kernel              # Build specific crate
cargo test --all                         # Run all tests
cargo test -p kaish-kernel --lib lexer   # Lexer tests only
cargo test -p kaish-kernel --lib parser  # Parser tests only
cargo test --features proptest -- --ignored  # Property tests
cargo tarpaulin --out Html --output-dir coverage/  # Coverage
cargo +nightly fuzz run parser -- -max_len=4096    # Fuzz (nightly)
```

If Cap'n Proto schema changes don't trigger rebuilds:
```bash
cargo clean -p kaish-schema && cargo build -p kaish-schema
```

## Development Guidelines

### Error Handling

- Use `anyhow::Result` for fallible operations
- Never use `unwrap()` — propagate with `?`
- Add context: `.context("what we were trying to do")`
- Never discard errors with `let _ =`

### Code Style

- Correctness and clarity over performance
- No summary comments — code should be self-explanatory
- Comments only for non-obvious "why"
- Avoid `mod.rs` — use `src/module_name.rs`
- Full words for names, no abbreviations
- Prefer newtypes over primitives: `struct JobId(Uuid)` not `Uuid`
- Use enums for states and variants
- Define traits for shared capabilities

### Async Patterns

Everything runs on tokio. For blocking operations in async contexts:
```rust
let state = tokio::task::block_in_place(|| self.state.blocking_write());
```

### Version Control

- **Never `git add .` or `git add -A`** — always explicit paths
- Review with `git status` before and after staging
- Use `git diff --staged` before committing
- Run `cargo test` before committing

### Commit Attribution

```
Co-Authored-By: Claude <claude@anthropic.com>
```

## Architecture

The 核 (kaku/kernel) is the unit of execution. Multiple frontends connect to the same kernel:

```
Frontends (REPL, Script Runner, MCP Server, Embedded)
    ↓ KernelClient trait
        ├── EmbeddedClient (direct in-process)
        └── IpcClient (Unix socket + Cap'n Proto RPC)
    ↓
Kernel (核)
    ├── Lexer (logos)
    ├── Parser (chumsky)
    ├── Interpreter (tokio async)
    ├── Tool Registry (builtins + MCP)
    ├── VFS Router (mount points)
    ├── Job Scheduler (background jobs, scatter/gather)
    └── SQLite State (persistence)
```

### Crate Structure

```
crates/
├── kaish-schema/    # Cap'n Proto codegen from schema/kaish.capnp
├── kaish-kernel/    # Core: lexer, parser, interpreter, tools, VFS
├── kaish-client/    # Client implementations (embedded, IPC)
├── kaish-repl/      # Interactive REPL with rustyline
└── kaish-mcp/       # MCP server frontend
```

### Build Strategy

See `docs/BUILD.md` for the 14-layer bottom-up implementation plan. All layers are complete.

## Language Key Points

**Bourne-compatible syntax:**

- `VAR=value` — assignment (no spaces around `=`)
- `$VAR` and `${VAR}` — both work for expansion
- `${VAR:-default}` — default values
- `${#VAR}` — string length
- `$0`-`$9`, `$@`, `$#` — positional parameters
- `'literal'` and `"interpolated"` — both quote styles
- `[[ ]]` — test expressions
- `if/elif/else/fi`, `for/do/done`, `while/do/done` — control flow
- `break`, `continue`, `return`, `exit` — control statements
- `set -e` — exit on error mode
- `source file` or `. file` — script sourcing
- `-x`, `--flag` — flag arguments
- `key=value` — named arguments

**Kaish-specific:**

- 散/集 (scatter/gather) for parallel execution
- User-defined tools with typed parameters
- MCP tool integration
- VFS mounts
- Export scripts as MCP servers

### What's Intentionally Missing

Arithmetic `$(( ))`, brace expansion `{a,b,c}`, glob expansion `*.txt`, here-docs `<<EOF`, process substitution `<(cmd)`, backticks, aliases, `eval`, arrays of arrays

## Testing Strategy

Target: **10:1 test-to-feature ratio** (~700 tests total)

Test files in `tests/`:
- `lexer/tokens.txt` — line-separated token tests
- `parser/*.test` — markdown-like format with expected AST
- `eval/*.test` — scripts with expected stdout/stderr/exit

## Key Documentation

| File | Purpose |
|------|---------|
| `docs/LANGUAGE.md` | Full language specification |
| `docs/GRAMMAR.md` | EBNF grammar, ambiguity analysis |
| `docs/ARCHITECTURE.md` | Kernel design, crate structure, protocols |
| `docs/BUILD.md` | Layered build plan, dependencies |
| `docs/TESTING.md` | Testing strategy and requirements |
| `docs/kanji.md` | Kanji vocabulary for the project |

## Schema Files

- `schema/kaish.capnp` — Cap'n Proto schema (kernel protocol, types)
- `schema/state.sql` — SQLite schema (kernel state persistence)
