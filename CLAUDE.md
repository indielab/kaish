# CLAUDE.md

## Project Overview

**kaish** (会sh — "the gathering shell") is a predictable shell for AI agents, exposed as an MCP server.
Part of [Kaijutsu](https://github.com/tobert/kaijutsu).

**Status**: Core implementation complete. Lexer, parser, interpreter, builtins, MCP server, VFS.

**Philosophy**: 80% of a POSIX/Bourne/bash shell, 100% unambiguous. Bourne-compatible subset passes `shellcheck --enable=all`. Extensions (floats, typed params, scatter/gather) are outside ShellCheck's scope.

**What's intentionally missing**: process substitution `<(cmd)`, backticks, `eval`

## Build Commands

```bash
cargo build                              # Build workspace
cargo build -p kaish-kernel              # Build specific crate
cargo test --all                         # Run all tests
cargo test -p kaish-kernel --test lexer_tests   # Lexer tests only
cargo test -p kaish-kernel --test parser_tests  # Parser tests only
cargo insta test                         # Run snapshot tests
cargo insta test --check                 # CI mode (fails on pending snapshots)
cargo insta review                       # Interactive review of pending snapshots
```

## Development Guidelines

### Error Handling

- Use `anyhow::Result` for fallible operations
- Avoid `unwrap()` — propagate with `?`
- Add context: `.context("what we were trying to do")`
- Never discard errors.
   - If an error can never happen in practice it can be hidden, but the program must panic on the outside case.
   - When an error is explicitly ignored, it must have a comment saying so.

### Code Style

- Comments only for non-obvious "why"
- Avoid `mod.rs` — use `src/module_name.rs`
- Full words for names, avoid abbreviations
- Tokio for all async. Blocking in async: `tokio::task::block_in_place(|| ...)`

### Version Control

- **Always add files by name** — no `git add -A` or `git add .`
- Run `cargo test` before committing
- Models include attribution: `Co-Authored-By: Claude <claude@anthropic.com>`

## Architecture

The 核 (kaku/kernel) is the unit of execution. Multiple frontends connect to the same kernel:

```
Frontends (REPL, Script Runner, Embedded)
    ↓ KernelClient trait
        └── EmbeddedClient (direct in-process)
    ↓
Kernel (核)
    ├── Lexer (logos)
    ├── Parser (chumsky)
    ├── Validator (pre-execution checks)
    ├── Interpreter (tokio async)
    ├── Tool Registry (builtins + user tools)
    ├── VFS Router (local, memory, git backends)
    └── Job Scheduler (background jobs, scatter/gather)
```

### Crate Structure

```
crates/
├── kaish-types/     # Pure-data leaf crate: OutputData, ExecResult, Value, DirEntry, etc.
├── kaish-glob/      # Glob matching and async file walking with gitignore support
├── kaish-kernel/    # Core: lexer, parser, interpreter, tools, VFS, validator
├── kaish-mcp/       # MCP server (expose kaish as an MCP tool)
├── kaish-client/    # Client implementations (embedded)
└── kaish-repl/      # Interactive REPL with rustyline
```

The MCP server binary accepts `--init <path>` (repeatable) to load `.kai` scripts before each `execute()` call.

## Testing

Uses **rstest** for parameterized tests and **insta** for snapshot testing.
Tests live in `crates/kaish-kernel/tests/`. Snapshots in `snapshots/*.snap`.

## Documentation

- `docs/LANGUAGE.md` — complete language reference
- `docs/help/*.md` — help system content, embedded at compile time into the kernel

**Keep in sync:** When adding builtins or changing syntax, update the relevant help files.
The builtin list in `help builtins` is generated dynamically from tool schemas, but
`syntax.md` and `limits.md` need manual updates.
