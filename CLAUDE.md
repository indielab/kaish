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
- **ShellCheck-clean** — the Bourne subset passes `shellcheck --enable=all`
- **Agent-friendly** — easy to generate, parse, validate
- **Fail fast** — ambiguity is an error, not a guess

### ShellCheck-Clean Design

The Bourne-compatible subset of kaish should pass `shellcheck --enable=all`.
When implementing features, verify they don't introduce constructs ShellCheck
would warn about. Extensions (floats, typed params, MCP tools, scatter/gather)
are outside ShellCheck's scope and clearly marked.

## Build Commands

```bash
cargo build                              # Build workspace
cargo build -p kaish-kernel              # Build specific crate
cargo test --all                         # Run all tests
cargo test -p kaish-kernel --test lexer_tests   # Lexer tests only
cargo test -p kaish-kernel --test parser_tests  # Parser tests only
cargo insta test                         # Run snapshot tests
cargo insta test --check                 # CI mode (fails on pending snapshots)
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

Arithmetic `$(( ))`, brace expansion `{a,b,c}`, glob expansion `*.txt`, here-docs `<<EOF`, process substitution `<(cmd)`, backticks, aliases, `eval`

## Testing Strategy

Uses **rstest** for parameterized tests and **insta** for snapshot testing.

Test files in `crates/kaish-kernel/tests/`:
- `lexer_tests.rs` — rstest parameterized lexer tests (~123 tests)
- `parser_tests.rs` — insta snapshot tests for AST output (~83 tests, 16 ignored)
- `eval_tests.rs` — rstest eval tests (all ignored until interpreter ready)
- `snapshots/*.snap` — insta snapshot files for parser tests

Snapshot workflow:
```bash
cargo insta test           # Run tests, create .snap.new for changes
cargo insta review         # Interactive review of pending snapshots
cargo insta accept         # Accept all pending snapshots
```

## Key Documentation

| File | Purpose |
|------|---------|
| `README.md` | Language reference, syntax, builtins, ShellCheck alignment |
| `docs/GRAMMAR.md` | EBNF grammar, ambiguity analysis |
| `docs/ARCHITECTURE.md` | Kernel design, crate structure, protocols |
| `docs/kanji.md` | Kanji vocabulary for the project |

## Schema Files

- `schema/kaish.capnp` — Cap'n Proto schema (kernel protocol, types)
- `schema/state.sql` — SQLite schema (kernel state persistence)
