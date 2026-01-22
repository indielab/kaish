# ShellCheck Alignment Guide

## Design Principle

**The Bourne-compatible subset of kaish passes `shellcheck --enable=all`.**

This is a core design decision, not an accident. By building a shell language where ShellCheck can't find anything to warn about, we've eliminated entire classes of bugs at the language level. Features that ShellCheck flags as problematic in bash simply don't exist in kaish.

**Extensions are explicit.** Kaish adds floats, objects, MCP tools, and scatter/gather — features ShellCheck doesn't know about. These extensions use distinct syntax and are clearly documented, so there's no confusion about what's portable shell and what's kaish-specific.

---

## Warnings Eliminated by Design

These ShellCheck warnings are **impossible to trigger** in kaish because the language doesn't include the problematic constructs:

| SC Code | Warning | Kaish Approach |
|---------|---------|----------------|
| **SC2006** | Use `$()` instead of backticks | Backticks forbidden — only `$()` exists |
| **SC2086** | Double quote to prevent globbing and word splitting | No word splitting — variables expand to single values |
| **SC2046** | Quote this to prevent word splitting | No word splitting — `$(cmd)` returns a single value |
| **SC2035** | Use `./*` so globs don't expand to flags | No glob expansion — tools handle their own patterns |
| **SC2144** | Use `[[ -e ]]` for multi-item globs | No glob expansion |
| **SC2039** | Use `[[ ]]` in POSIX sh | Only `[[ ]]` exists — no `[ ]` |
| **SC2069** | fd order matters for `2>&1` | No fd duplication — use `&>` for combined output |
| **SC1083** | Escape literal braces | No brace expansion — `{a,b,c}` is syntax error |
| **SC2004** | `$` is redundant in `$(())` | No arithmetic expansion — use tools for math |
| **SC2015** | `A && B || C` is not if-then-else | Documented clearly; kaish users understand this |
| **SC2162** | `read` without `-r` mangles backslashes | No `read` builtin — tools handle input |
| **SC2129** | Use `{}` instead of multiple redirects | Redirects work differently in kaish |
| **SC2164** | Use `cd ... || exit` for error handling | `set -e` is explicit; CD failures can be fatal |
| **SC2012** | Use `find` instead of `ls` for iteration | Tools return structured data, not parsed text |
| **SC2091** | Remove surrounding `$()` for literal text | Context is always clear — no surprise expansion |

### Word Splitting: A Class of Bugs, Gone

In bash, unquoted variable expansion triggers word splitting and glob expansion:

```bash
# Bash: this is dangerous
FILES="foo bar"
rm $FILES      # Deletes 'foo' and 'bar' as separate args!

FILES="*.txt"
echo $FILES    # Expands glob!
```

In kaish, variables are single values:

```bash
# Kaish: safe by default
FILES="foo bar"
rm $FILES      # rm receives one arg: "foo bar"

FILES="*.txt"
echo $FILES    # Prints literal: *.txt
```

This eliminates SC2086, SC2046, SC2035, SC2144, and dozens of related warnings.

### Deprecated Syntax: Not Implemented

| SC Code | Deprecated | Kaish |
|---------|------------|-------|
| SC2006 | `` `cmd` `` | Use `$(cmd)` |
| SC2039 | `[ ]` tests | Use `[[ ]]` |
| SC1073 | `function name()` | Use `tool name { }` |
| SC2112 | `function` keyword | Use `tool` keyword |

---

## Warnings Not Applicable

These ShellCheck warnings don't apply because kaish has different semantics:

| SC Code | Warning | Why N/A in Kaish |
|---------|---------|------------------|
| SC2004 | `$` unnecessary in `$(())` | No arithmetic expansion exists |
| SC2068 | Quote `$@` to pass args | `$@` is a single array value |
| SC2128 | `$array` expands to first element | Arrays are first-class — `$arr` is the whole array |
| SC2034 | Unused variable | Kaish doesn't track usage (yet) |
| SC2154 | Variable referenced but not assigned | Runtime error, not static warning |
| SC2236 | Use `-n` instead of `! -z` | Both work identically in `[[ ]]` |

---

## Extensions Beyond ShellCheck's Scope

These kaish features are outside what ShellCheck validates. They have no bash equivalent, so ShellCheck doesn't know about them:

| Feature | Example | Notes |
|---------|---------|-------|
| **Floats** | `PI=3.14159` | POSIX shells are integer-only |
| **Booleans** | `ENABLED=true` | Strict `true`/`false` only |
| **Objects** | `CONFIG={host: "x"}` | JSON-compatible object literals |
| **Arrays** | `ITEMS=[1, 2, 3]` | JSON-style, not bash-style |
| **Typed params** | `tool foo x:int { }` | Tool parameter typing |
| **Scatter/gather** | `散/集` pipeline ops | Parallel execution primitives |
| **VFS paths** | `/mcp/server/resource` | Virtual filesystem mounts |
| **MCP tools** | `exa.web_search query=...` | External tool invocation |
| **Object access** | `${OBJ.field}` | Dot notation for nested access |
| **Array index** | `${ARR[0]}` | Bracket notation (different from bash) |

**These features are explicitly marked in documentation** with ⚠️ EXTENSION labels.

---

## Validation Strategy

### What We Validate Statically

Kaish's parser catches many issues that ShellCheck would warn about:

```bash
# Parser rejects these (ShellCheck warnings become parse errors)
if="value"           # Error: 'if' is a keyword
cmd key = value      # Error: unexpected '=' (spaces not allowed)
TRUE                 # Error: use lowercase 'true'
.5                   # Error: use '0.5'
```

### Runtime Validation

```bash
# Runtime errors for undefined variables
echo $UNDEFINED      # Error: undefined variable 'UNDEFINED'

# Strict boolean checking
FLAG=YES             # Error: ambiguous boolean (use 'true')
FLAG=1               # Error: ambiguous boolean (use 'true')
```

### CI Validation

Run ShellCheck on Bourne-subset test cases:

```bash
./scripts/shellcheck-bourne-subset.sh
```

This extracts tests marked `# bourne: yes` and verifies they pass `shellcheck --enable=all`.

---

## Mapping: ShellCheck Rules to Kaish Design

### Critical (Error-Level) Rules

| SC Code | Bash Problem | Kaish Solution |
|---------|-------------|----------------|
| SC2006 | Backticks nested poorly | Only `$()` allowed |
| SC2086 | Word splitting attacks | No word splitting |
| SC2039 | `[ ]` behaves differently | Only `[[ ]]` |
| SC1083 | Brace expansion surprises | No brace expansion |

### Warning-Level Rules

| SC Code | Bash Problem | Kaish Solution |
|---------|-------------|----------------|
| SC2046 | `$(cmd)` splits output | Single-value expansion |
| SC2035 | `*` becomes flags | No glob expansion |
| SC2012 | `ls` output parsing | Tools return structured data |
| SC2162 | `read` backslash handling | No `read` builtin |

### Style Rules

| SC Code | Recommendation | Kaish Approach |
|---------|---------------|----------------|
| SC2028 | `echo` escapes | Use `\n` in double quotes |
| SC2059 | `printf` format string | Not applicable — use tools |
| SC2129 | Group redirects | Single redirect model |

---

## Testing ShellCheck Alignment

### Test File Markers

Test files use `# bourne: yes` to mark Bourne-compatible tests:

```
# test: simple_assignment
# expect: ok
# bourne: yes
---
NAME="value"
---
```

Tests without this marker are kaish extensions and aren't validated against ShellCheck.

### Running Validation

```bash
# Validate all Bourne-subset tests
./scripts/shellcheck-bourne-subset.sh

# Check a specific file manually
shellcheck --enable=all --shell=sh script.sh
```

### What Passes

```bash
# These pass shellcheck --enable=all
NAME="value"
echo "$NAME"
if [[ -f /etc/hosts ]]; then echo "found"; fi
for item in "$@"; do echo "$item"; done
cmd1 && cmd2 || echo "failed"
```

### What Kaish Has That ShellCheck Doesn't Know About

```bash
# Kaish extensions (not checked by ShellCheck)
PI=3.14159                                    # Floats
CONFIG={host: "localhost", port: 8080}        # Objects
cat data | scatter | process | gather         # 散/集
exa.web_search query="rust parser"            # MCP tools
```

---

## Philosophy: Why This Matters

### For Human Authors

ShellCheck is the de facto linter for shell scripts. By designing kaish to align with ShellCheck's recommendations, we ensure that:

1. **Skills transfer** — Bash users' ShellCheck-trained instincts work in kaish
2. **No unlearning** — Features ShellCheck warns against don't exist
3. **Clear boundaries** — Extensions are obviously different syntax

### For AI Agents

AI agents generate shell-like code. ShellCheck alignment means:

1. **Generation is safer** — Agents can't generate word-splitting bugs
2. **Validation is simple** — Run ShellCheck on generated Bourne subset
3. **Errors are clear** — Kaish rejects ambiguous input with helpful messages

### For Tooling

ShellCheck alignment enables:

1. **Editor integration** — ShellCheck plugins work on kaish's Bourne subset
2. **CI pipelines** — Existing ShellCheck CI jobs validate kaish scripts
3. **Migration paths** — Bash scripts can be incrementally ported

---

## Reference: Key ShellCheck Categories

For reference, here are ShellCheck's warning categories relevant to kaish design:

### Quoting (SC20xx)
Most eliminated by no word splitting.

### Deprecated/Obsolete (SC2xxx)
Backticks, single brackets — simply not in the grammar.

### Style (SC2xxx)
Where applicable, kaish follows the recommended style by default.

### Portability (SC2xxx)
Kaish doesn't claim POSIX compliance — it's explicitly Bourne-lite with extensions.

---

*See also: [Language Specification](LANGUAGE.md), [Formal Grammar](GRAMMAR.md)*
