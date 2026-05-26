# clap_derive migration recipe

Status: **pilot complete** — `echo`, `jobs`, `cat` use clap_derive internally.
~60 remaining builtins to migrate. The `Tool` trait signature is unchanged;
clap is a private implementation detail inside each `execute()`.

## Why clap?

Each builtin used to declare a `ToolSchema` **and** write hand-rolled argv
extraction (`args.has_flag("number") || args.has_flag("n")`, `args.get_string(...)`).
Two sources of truth, drifting in opposite directions. clap_derive collapses
the argv side into one struct.

## The adapter: `ToolArgs::to_argv()`

The kernel does shell parsing for us — by the time a builtin is invoked, variables
are expanded, globs are expanded, `$(...)` is substituted, and schema-driven
flag-vs-value splitting has happened. `ToolArgs::to_argv()` (in `kaish-types`)
flattens the resulting `ToolArgs` back into a `Vec<String>` argv that clap can parse.

Layout produced:

1. **Flags first**, sorted alphabetically, single-char keys as `-x`, multi-char
   as `--name`. Sort is deterministic so snapshot tests stay stable.
2. **Named values next**, in BTreeMap order, as `-k=value` or `--key=value`.
   The `=` form keeps clap unambiguous when the value begins with `-`.
3. **`--` terminator**, then positionals in order. The double-dash means clap
   accepts positionals beginning with `-` (e.g. `echo -- -n` prints `-n`).

Single-char-vs-multi-char rendering matters: the kernel stores `-n` as
`flags: {"n"}` (canonical = the literal char) and `--no_newline` as
`flags: {"no_newline"}`. Rendering single-char as short form lets clap's natural
`#[arg(short = 'n', long = "no_newline")]` derive accept both kernel encodings
without `visible_alias` gymnastics.

## The recipe (per builtin)

```rust
use clap::{CommandFactory, Parser};

use crate::interpreter::{ExecResult, OutputData};
use crate::tools::{schema_from_clap, ExecContext, GlobalFlags, Tool, ToolArgs, ToolSchema};

/// clap-derived argv layer for foo. See docs/clap-migration.md.
#[derive(Parser, Debug)]
#[command(name = "foo", about = "Short description used in help")]
struct FooArgs {
    /// Number output lines.
    #[arg(short = 'n', long = "number")]
    number: bool,

    #[command(flatten)]
    global: GlobalFlags,

    /// Sink — to_argv() always emits `--` before positionals, so clap
    /// accepts arbitrary tokens. Read paths off args.positional directly.
    #[arg(hide = true)]
    paths: Vec<String>,
}

#[async_trait]
impl Tool for Foo {
    fn name(&self) -> &str { "foo" }

    fn schema(&self) -> ToolSchema {
        schema_from_clap(
            &FooArgs::command(),
            "foo",
            "Short description used in help",
            [
                ("Example one", "foo --number"),
                ("Example two", "foo --number file.txt"),
            ],
        )
    }

    async fn execute(&self, args: ToolArgs, ctx: &mut ExecContext) -> ExecResult {
        let parsed = match FooArgs::try_parse_from(
            std::iter::once("foo".to_string()).chain(args.to_argv()),
        ) {
            Ok(p) => p,
            Err(e) => return ExecResult::failure(2, format!("foo: {e}")),
        };
        parsed.global.apply(ctx);

        // Use parsed.number for flags.
        // Use args.positional for Value-typed positionals — see below.
    }
}
```

### Conventions

- **`#[command(name = "<tool>", about = "...")]`** — set both. `name` makes
  clap's help read naturally; `about` is the short description, hand-fed back
  into `schema_from_clap` so the dispatcher and `help <tool>` agree. Do **not**
  set `no_binary_name = true`; we prepend the tool name as the binary slot.
- **Always `#[command(flatten)] global: GlobalFlags`** for the `--json` flag.
  Call `parsed.global.apply(ctx)` after the parse so the dispatcher picks up
  `ctx.output_format`.
- **Schema via `schema_from_clap`.** Params are derived from the clap struct;
  description and examples remain hand-written (clap doesn't own those).
  `params_from_clap` filters out `help`/`version`/`json` and any
  `#[arg(hide = true)]` sink fields.
- **Parse failure → exit 2** — POSIX usage convention. `failure(2, format!("<tool>: {e}"))`.
- **No `trailing_var_arg` / `allow_hyphen_values` needed.** `to_argv()` always
  emits `--` before positionals, so clap natively accepts hyphenated values
  there. Adding the attributes is noise.
- **Tests need no changes.** They construct `ToolArgs` directly and call
  `execute()` — the `to_argv()` reconstruction is byte-identical for the cases
  existing tests cover. If a test asserts specific clap-style error text,
  it's already migrated and you should leave it alone.

### Positionals: clap as a guard, `args.positional` as the source

clap parses `args.to_argv()` as `Vec<String>`. `to_argv()` stringifies values
(`Value::Int(3) → "3"`, `Value::Blob(...) → "[blob: ...]"`), which is fine for
argv but **lossy** for downstream logic that needs the original Value.

The pattern used by every pilot builtin:

1. **Declare a sink positional field** in the clap struct (`paths: Vec<String>`,
   marked `#[arg(hide = true)]`). This makes clap accept the positionals
   without complaining about unexpected arguments.
2. **Never read the sink** in the body. Read `args.positional` directly when
   you need Value-typed positionals — that preserves `Value::Int`, `Value::Bool`,
   `Value::Blob`, etc.

This split gives you clap's flag validation (unknown flags → exit 2) while
keeping the semantic richness of `Value` for downstream code.

If a builtin renders values type-specifically (e.g. `echo true` should print
`"true"`, `echo 3.14` should print `"3.14"`), do the rendering off
`args.positional` directly. The `echo` builtin demonstrates this.

## Edge cases — flag, don't paper over

- **Trailing passthrough** (`xargs`, `timeout <cmd>`, `exec`): use
  `#[arg(trailing_var_arg = true, allow_hyphen_values = true)]` on the variadic
  positional field. Clap stops parsing flags once the first positional appears.
- **Intentional unknown-flag tolerance**: only set
  `#[command(allow_external_subcommands)]` or similar when an existing test
  proves the intent. Don't add escape hatches blindly.
- **Domain-specific arg parsing** (sed expressions, awk programs, find
  predicates, grep regex flags): stays hand-rolled. clap covers the argv
  layer; what those builtins do with their positionals is not argv parsing.

## Sweep decisions (Amy, 2026-05-26)

1. **Schema source of truth**: derive params from clap. No migration path —
   delete the hand-written `ParamSchema` declarations as each builtin migrates.
   `schema_from_clap` keeps description and examples hand-fed since clap doesn't
   own them.
2. **`--json` placement**: pushed into clap via `GlobalFlags` flatten. The
   legacy kernel pre-strip (`extract_output_format`) is kept active during the
   sweep so un-migrated builtins continue to work; it's removed in the final
   commit once every builtin is migrated.
3. **`key=value` argv shorthand**: audit found in-repo usage in
   `examples/scan.kai` and in `scatter`/`gather` example strings. Sweep
   converts these callsites to `--key value` form; once converted, the
   `Arg::Named` parser path is removed.

## Original checkpoint questions (resolved above)

1. **`fn schema()` — derive from clap, or keep hand-written?** The REPL
   completer and pre-execution validator both consume `ToolSchema` today.
   Recommendation: keep hand-written during migration so completion stays
   alive; revisit deriving from clap as a separate task once every builtin has
   migrated. The two sources of truth still exist *temporarily* — but only on
   the surface (one struct + one schema declaration) instead of being scattered
   through `args.has_flag(...)` calls.

2. **Global `--json` — keep pre-strip, or move into clap?** Today the kernel
   strips `--json` from `ToolArgs` before any tool sees it (`extract_output_format`
   in `tools/traits.rs`). Recommendation: keep the kernel pre-strip. It's a
   cross-cutting output-format concern, not a per-builtin flag; pushing it into
   every clap struct would be ~60 copies of the same boilerplate, and would
   change semantics for external commands (`cargo --json` already works because
   the kernel deliberately does not strip `--json` from un-schema'd commands).

3. **`named` for builtins — keep value-flags routing, or remove?** Today
   `head -n 5` lands as `named["n"] = 5` in `ToolArgs` (kernel uses the schema
   to know `-n` consumes a value). Value-flags ARE in scope for the sweep.
   But the `key=value` argv shorthand (e.g. `cat path=/foo.txt`) is silently
   broken today — `cat.rs` only iterates `positional`, never `named`. Amy's
   pilot decision: **back out the experiment**. Sweep TODO: audit which
   builtins actually need value-flags and migrate them via clap; remove the
   `key=value` argv shorthand for builtins entirely so the language stops
   shipping a broken surface.

## Binary size

To be measured in the final sweep PR description:

```bash
cargo build --release -p kaish-repl
ls -l target/release/kaish
```

Pilot delta is too small to be meaningful (3 builtins out of ~60); the
sweep-PR measurement is the real signal.
