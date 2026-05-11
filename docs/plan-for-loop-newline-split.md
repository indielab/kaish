# Plan: Newline-split for `$(cmd)` in for-loop position

Close the muscle-memory gap where humans and models reach for bash's
`for line in $(cat file)` line-iteration idiom, without resurrecting
implicit word splitting. Surgical, scoped, and intentionally
shellcheck-strict-compatible.

## The change in one sentence

When `$(cmd)` appears as a `for`-loop iteration item and its result is a
plain `Value::String` containing `\n`, split that string on `\n` (after
trimming trailing newlines) instead of treating it as one item.
Everywhere else — assignment, argv, string interpolation, `while`
conditions — strings stay whole.

## Motivation

Today, all of these iterate once with the whole blob concatenated:

```bash
for line in $(cat hosts.txt); do ssh $line; done
for hash in $(git log --format=%H | head); do git show $hash; done
for path in $(find . -name '*.rs'); do wc -l $path; done   # find emits .data, so this works; example shown for contrast
for tag in $(git tag); do echo $tag; done
```

…because `cat`, `git`, external commands generally, and several
non-`.data`-emitting builtins return their stdout as one string. The
existing escape hatch is `split` or `| jq -R`, which works but isn't
what muscle memory types.

Implicit word splitting is the wrong fix — it brings back every
quoting bug bash has, and it fails `shellcheck --enable=all`
(SC2086/SC2046). Newline-split-in-iteration-position is a much
narrower rule that captures the same intent without the footguns.

## Specification

### Scope

The new behavior applies **only** when all three conditions hold:

1. The expression sits in the iteration list of a `for` loop
   (`for X in <here>; do …; done`).
2. The expression is `Expr::CommandSubst(...)` — a `$(cmd)`
   substitution. Not `Expr::VarRef` (still bare-scalar E012), not
   `Expr::Literal`, not `Expr::Interpolated`.
3. The evaluated result is `Value::String(s)` — i.e., `.data` was
   *not* set. Builtins that emit `Value::Json(Array(...))` continue
   to spread element-by-element; the new path only fires when there's
   no structured data.

### Algorithm

Inside the `for`-iteration branch of `kernel.rs` (currently around
line 1515), the `Value::String(s)` arm becomes:

```rust
Value::String(s) if is_command_subst => {
    let trimmed = s.trim_end_matches(&['\n', '\r'][..]);
    if trimmed.contains('\n') {
        for line in trimmed.split('\n') {
            // Optionally trim trailing \r for CRLF stdout.
            let line = line.trim_end_matches('\r');
            items.push(Value::String(line.to_string()));
        }
    } else {
        items.push(Value::String(trimmed.to_string()));
    }
}
```

Notes:
- The `is_command_subst` predicate is decided when collecting items,
  by inspecting the AST kind of `item_expr` before evaluation.
- Trim trailing newlines once before splitting so the conventional
  trailing `\n` from Unix commands doesn't produce a phantom empty
  item.
- Don't trim interior empty lines — `printf 'a\n\nb\n'` should yield
  `["a", "", "b"]`. A user who wanted empty lines stripped can pipe
  through `grep .` first.
- Whitespace within a line is *never* split. `$(echo "hello world")`
  has no newline, so it iterates once as `"hello world"`.

### What does NOT change

- `for i in $VAR` — still E012 hard error. The bareword scalar case
  has no `\n`-list-stdout signal to lean on, and disambiguating
  one-iteration vs. split is genuinely impossible there. Tell the
  user to use `split` or `$(cmd)` explicitly.
- `for i in "$(cmd)"` — quoted substitution stays one item, matching
  bash's `IFS=` discipline.
- `cmd $(other_cmd)` — argv position. One arg with possible newlines
  inside. The "no implicit word splitting in argv" promise is intact.
- `VAR=$(cmd)` — assignment. `VAR` is the whole stdout as a string,
  with trailing `\n` stripped (current behavior).
- `"prefix $(cmd) suffix"` — string interpolation. One concatenated
  string.
- `while cond; do …; done` — `while` evaluates its condition each
  iteration; it's not a foreach. No iteration list, no newline-split
  surface. (See "Why not while?" below.)
- Builtins that already emit `.data` (seq, jq, cut, find, glob,
  split) — `.data` is checked first in CommandSubst evaluation, so
  their structured-array iteration is unaffected.

## Examples

### Now iterates per line

```bash
for line in $(cat hosts.txt); do ssh $line; done
for hash in $(git log --format=%H | head); do git show $hash; done
for tag in $(git tag); do echo $tag; done
for ref in $(git for-each-ref --format='%(refname)'); do echo $ref; done
for line in $(printf 'a\nb\nc'); do echo "[$line]"; done
# → [a] / [b] / [c]
```

### Still iterates once (no newline)

```bash
for x in $(echo "a b c"); do echo $x; done
# → a b c   (one iteration)

for x in $(date); do echo $x; done
# → Sun May 11 14:03:22 PDT 2026   (one iteration)
```

### Still spreads via `.data`

```bash
for n in $(seq 1 3); do echo $n; done           # 1 / 2 / 3
for f in $(find . -name '*.rs'); do echo $f; done
for name in $(echo '[1,2,3]' | jq -r '.[]'); do echo $name; done
```

### Quoting still suppresses splitting

```bash
for x in "$(printf 'a\nb\nc')"; do echo "[$x]"; done
# → [a\nb\nc]   (one iteration, newlines preserved)
```

### Trailing newlines don't create phantoms

```bash
for x in $(printf 'a\nb\n'); do echo "[$x]"; done
# → [a] / [b]   (not [a] / [b] / [])
```

### Empty stdout

```bash
for x in $(printf ''); do echo "[$x]"; done
# → (zero iterations)
```

### Interior empty lines preserved

```bash
for x in $(printf 'a\n\nb\n'); do echo "[$x]"; done
# → [a] / [] / [b]
```

## Why not `while`?

`while <condition>; do …; done` does not have an iteration list — the
condition is re-evaluated each pass and the body runs as long as it's
truthy. There's no analogue to the `for` items where multi-line
stdout could naturally turn into per-iteration values.

The bash idiom that *does* iterate via `while`:

```bash
while IFS= read -r line; do …; done < file
```

…relies on the `read` builtin consuming one line per call from a
redirected stdin, not on substitution. If we ever want a kaish
equivalent, that's a separate feature (a line-reading builtin or a
`read` analogue), not a substitution rule.

So: `while` is intentionally out of scope.

## Compatibility with existing principles

| Principle | Impact |
| --- | --- |
| No implicit word splitting | Preserved. Whitespace within a line never splits. |
| No JSON sniffing (`arch_no_json_sniffing.md`) | Preserved. We split on a single byte (`\n`) keyed on AST position, not by inspecting content for structure. |
| `.data` is opt-in (`arch_data_iteration.md`) | Preserved. `.data` is checked first; newline-split is the fallback. |
| shellcheck `--enable=all` compatibility | Strengthened. The bash-strict line-iter idiom is `while read; do … done < <(cmd)`, which kaish doesn't support; this gives users a strict-clean alternative. |
| "$VAR with spaces just works" (MCP marketing) | Preserved. Variables stay whole everywhere. |

## Edge cases and pathological inputs

- **Filenames with embedded newlines.** Same footgun bash has;
  there's no general fix without spec'ing a separator. `find -print0`
  / `xargs -0` analogues are a separate problem. Accept.
- **Multi-line JSON from an external command in for-iteration
  position.** e.g., `for x in $(cargo metadata)` — would split the
  pretty-printed JSON into broken fragments. But: this already
  doesn't work today (returns one giant string and iterates once,
  which is also useless). Users must `| jq -c '.packages[]'` either
  way. No regression. The future typed-substitution work (see memory
  `lang_typed_substitution_future.md`) is the proper fix.
- **Multi-line `cat config.toml`.** Would split per line, which isn't
  what you want for parsing. Users normally assign to a variable
  (no split, by spec) and pipe through a parser. Accept; document.
- **CRLF stdout.** The split also trims trailing `\r` from each line
  so Windows-origin files iterate cleanly.

## Test plan

Failing tests staged in
`crates/kaish-kernel/tests/for_newline_split_tests.rs` before the
kernel change, in line with the project's TDD discipline. Each test
is a fact about the new spec, not a description of the
implementation.

Coverage:

1. `printf` multi-line → N iterations
2. `cat` of a tempfile with newlines → per-line iteration
3. `echo` single line with spaces → one iteration (preserve "no
   whitespace split")
4. External command with multi-line stdout (`git tag` against a real
   git repo? or `seq` via the system binary if available — needs a
   portable witness; fall back to a printf-based external invocation)
5. `.data` precedence: `seq 1 3` still iterates per element
6. Quoted substitution: `"$(printf 'a\nb')"` → one iteration with
   newlines preserved
7. Trailing newline: `printf 'a\nb\n'` → 2 iterations, not 3
8. Interior empty line: `printf 'a\n\nb\n'` → 3 iterations with
   empty middle
9. Empty stdout → 0 iterations
10. Single line no trailing newline → 1 iteration
11. CRLF stdout → per-line, no trailing `\r`
12. Assignment doesn't split: `R=$(printf 'a\nb'); echo "[$R]"` →
    one-line output containing the literal newline
13. Argv doesn't split: `echo $(printf 'a\nb')` → one arg, output
    contains the embedded newline (or echo concatenates per its
    semantics — assert presence, not exact form)
14. `while` condition unchanged (regression guard): a while with
    `$(cmd)` in the body still works as before
15. `for i in $VAR` still errors E012 (regression guard for the
    validator)

## Out of scope (deferred)

- Typed command substitution (parse JSON/etc. on capture). See
  memory `lang_typed_substitution_future.md` for two alternative
  designs.
- `read`-style line consumption via redirected stdin.
- `$(< file)` shortcut. Not implemented today; if/when added, it
  would flow through the same CommandSubst evaluator and inherit the
  newline-split behavior automatically.
- Configurable separator (à la `IFS`). Newline is the only conventional
  Unix list separator that doesn't conflict with payload content;
  exposing a knob just brings the IFS footgun back through the front
  door.

## Open questions

- Should the validator gain a *warning* (not error) when a
  for-iteration `$(cmd)` is statically a known no-`.data` builtin
  emitting structured content (e.g., the pipeline ends in
  `cargo`/`gh api`)? Deferred — too clever for the first pass. The
  newline-split rule does the right thing for the line-oriented
  case; structured-content iteration belongs to the typed-subst
  work.
- Documentation surface: this rule lands in `docs/LANGUAGE.md`
  (replacing the "iterates once" example), `help/limits.md` (which
  currently still says "use split when needed" — update the
  framing), and `help/overview.md` (the "$VAR is always one value"
  bullet stays correct, but the "for in $(cmd)" worked-example
  needs revision).
