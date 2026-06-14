# `date` upgrades — make the builtin match the muscle memory

> **Status: IMPLEMENTED 2026-06-14.** `builtin/date.rs` was rewritten against this
> spec — all three footguns closed, `-d`/`-I`/`-R`/`-r`/`--json`/`--tz` landed,
> `%N` translated, injectable `Clock` for testing. The one deviation from the
> recommendations below: §6 (`--json`) was shipped now rather than deferred behind
> a probe (Amy's call). See the Resolved entry in [issues.md](issues.md). This doc
> is kept as the design record.

*The current `date` builtin was designed against an **imagined** `date`. This doc
re-specs it against the **empirical** one — what language models (the primary
operators of kaish-in-MCP) actually type when they reach for `date` without
reading docs.*

Companion research: [`cross-model-eval.md`](cross-model-eval.md) is the precedent
for the methodology; the fleet experiment that seeded this doc is summarized in
[TL;DR](#the-empirical-spec) below.

> **Revision note (2026-06-14, second pass).** First drafted from the flagship-only
> survey, then revised after re-running the experiment on the lite tier (Gemini
> flash-lite, Gemma 4 31B / 26B-A4B, DeepSeek V4-flash — nine models total). What the
> fuller data changed, all in [§Proposed upgrades](#proposed-upgrades) and
> [§Test plan](#test-plan-tdd): (1) **`@N` epoch-decode is now a land-first quick win**,
> split out ahead of the hard `-d` grammar — high demand, trivial code, closes a
> silent footgun; (2) **`--json` is demoted to an unvalidated hypothesis** — *zero* of
> nine models reached for anything JSON-shaped, so the survey can't back it; (3) the
> **format path should translate the GNU specifiers models type (`%N`)**, not just
> reject; (4) the `-d` subset gained the **absolute-date-plus-offset** form models
> actually nest; (5) the **survey is the golden test corpus**. If you're already
> working off an earlier copy, re-read those two sections.

---

## TL;DR

We asked six models (Opus 4.8, Sonnet 4.6, Haiku 4.5, Gemini 3.1 Pro, DeepSeek
V4-Pro, and the orchestrating Opus) to list a dozen go-to `date` commands from
weights alone. Their muscle memory is **convergent and GNU-shaped**. We then ran
those go-tos against the real kaish 0.8.1 builtin. The result:

- The forms every model types either **work by accident** (`date +%s` succeeds
  only because chrono happens to support `%s`) or **fail loudly** (`-I`, `-d`,
  `-R` → clap "unexpected argument").
- The flags the builtin actually advertises (`--unix`, `--iso`, `--format`) are
  forms **no model ever typed**.
- Three behaviors are worse than a clean failure — one **panics the worker**, two
  return **wrong answers with exit 0**. These are filed in
  [`issues.md`](issues.md) under P1 and summarized [below](#the-three-footguns).

The cheap wins land first — `@N` epoch-decode, the `-I`/`-R` aliases, and
format-path hardening. The one genuinely hard piece is `-d`/`--date` relative
parsing — *also* the single highest-value feature, the trick the whole fleet reaches
for first. (`--json` was in the first draft's "cheap fix" list; the lite-tier data
demoted it to a separate, as-yet-unevidenced bet — see §6.)

---

## The empirical spec

What the fleet typed, ranked by how many of the six reached for it, mapped to what
kaish 0.8.1 actually does (probed live through `run_kaish` on 2026-06-14):

| Muscle-memory form | reached by | kaish 0.8.1 result | verdict |
|---|---|---|---|
| `date` | 6/6 | local `%Y-%m-%d %H:%M:%S` | ✅ works |
| `date -u` | 6/6 | UTC | ✅ works |
| `date +%s` | 6/6 | epoch (chrono `%s`) | ✅ works *by accident* |
| `date +%Y-%m-%d` / `+%F` | 5/6 | works via format path | ✅ works |
| `date -d "yesterday"` / `"2 weeks ago"` / `"next friday"` | 6/6 | clap reject, exit 2 | ❌ unsupported |
| `date -d "@1700000000"` (epoch → human) | 4/6 | **echoes `@1700000000`, exit 0** | ☠️ silent-wrong |
| `date -I` / `-Iseconds` | 3/6 | clap reject, exit 2 | ❌ unsupported |
| `date -R` (RFC 2822) | 3/6 | clap reject, exit 2 | ❌ unsupported |
| `date +%s%N` (nanos) | 2/6 | **panic → worker drop** | ☠️ crash |
| `date -r FILE` (mtime) | 2/6 | clap reject, exit 2 | ❌ unsupported |
| `TZ=zone date` | 3/6 | **ignores TZ, exit 0** | ☠️ silent-wrong |
| `--unix` / `--iso` / `--format` (what the builtin advertises) | 0/6 | works | 🤷 nobody types these |

The shared blind spot worth naming for kaish's own design philosophy: **every model
confessed a GNU/Linux bias and not one corrected for it.** Six independently-sampled
models share one muscle memory and one place they'd all get bitten (BSD/macOS, where
`-d` means "set" and date math is `-v`). kaish gets to *choose* which `date` it is —
and the empirical answer is "the GNU one, because that's what the operators expect."

**The footguns hit our own cheapest operator hardest.** Re-running the survey on the
lite tier (Gemini flash-lite, Gemma 4 31B / 26B-A4B, DeepSeek V4-flash) showed *no*
quality cliff — and the monoculture got *tighter*: `-d @N` epoch decode and `-d`
relative were 4/4 among the small models (vs. patchier among flagships). That matters
here because **kaibo's `explore` slot runs a Gemma-class model** — the exact model
class that reaches hardest for the two forms that are broken today (`-d @N` silently
echoes with exit 0; `-d` relative clap-rejects). The audience for these fixes isn't
hypothetical; it's the cheap explorer the consult split exists to lean on.

## The three footguns

These are the parts that violate "fail loud, not silent" and "crashing is preferred
over data corruption." Logged in [`issues.md`](issues.md) (P1); repros here so the
fix has a target.

1. **`date +%s%N` panics the worker.** chrono has no `%N` specifier → `format()`
   yields an `Item::Error` → `DelayedFormat`'s `Display` returns `fmt::Error` →
   `.to_string()` panics ("a Display implementation returned an error
   unexpectedly"). The panic drops the in-flight MCP reply (caller sees a cryptic
   `-32603 kaish worker dropped the reply`); the worker recovers on the next call,
   so it's a per-call DoS, not a server-killer. **Any** unknown specifier does this,
   so it's a wide trapdoor under a normal-looking command.

2. **`date "@1700000000"` echoes the literal with exit 0.** The arg help documents
   "`@TIMESTAMP`" but `execute()` never parses `@` — it falls through to the format
   path, strftime's nothing, and prints `@1700000000` verbatim. A model asking "what
   is epoch 1700000000?" gets `@1700000000` back and a success code. (Compounding:
   kaish's lexer rejects a bare `@`, so the documented form is also untypeable
   unquoted — the feature is triply dead: untypeable, unparsed, and silently wrong
   when forced through.)

3. **`TZ=zone date` silently ignores the zone.** `Local::now()` reads chrono's
   cached process offset; a kaish per-command env assignment never reaches it.
   `TZ=Asia/Tokyo date +%H:%M` returned Eastern local time (verified: 09:04 local
   vs 13:04 UTC, Tokyo should be 22:04), exit 0. Wrong answer, confident code.

---

## Proposed upgrades

Scoped to kaish's philosophy: **cover the convergent 90% the fleet actually types,
make the long tail fail loudly.** Not a GNU `date` clone. Ordered by value-to-effort
— the lite-tier data reordered this from the first draft (epoch-decode up front,
`--json` to the back).

### 1. `@N` epoch-decode — land this first

The best value-to-effort item in the doc: near-universal demand (4/4 lite, 5/6
flagship), ~5 lines of code (`@1700000000` → parse `i64` → `Utc.timestamp_opt()` →
format), and it closes footgun #2 on its own — no dependency on the hard `-d`
grammar, so it ships independently:

- A leading-`@` argument parses as epoch seconds and formats like `now`, honoring
  `-u` and any `+FORMAT`: `date -u "@1700000000"` → `2023-11-14T22:13:20Z`.
- A malformed `@` arg is a **loud parse error**, never an echo (kills the silent-
  wrong path directly).
- Lexer note: a bare `@` is rejected by kaish's lexer, so the typed form is quoted
  (`date "@N"`) or arrives via `-d "@N"` once §4 lands — the decoder serves both.

### 2. Alias the muscle-memory spellings (cheap)

clap flag/alias additions, all backed by chrono formatters already in the tree:

- `-I[FMT]` / `--iso-8601[=FMT]` where `FMT ∈ {date, hours, minutes, seconds, ns}`,
  defaulting to `date`. Keep `--iso` as a hidden alias for back-compat. (`-I` was
  typed across both tiers; `--iso` by nobody.)
- `-R` / `--rfc-2822` → `to_rfc2822()`.
- `--rfc-3339=FMT` for completeness (cheap once `-I` infra exists).
- Keep `+%s` working; demote `--unix` / `--iso` / `--format` to hidden aliases —
  **0/9 models typed them**, so they're surface area for confusion, not ergonomics.

### 3. Harden the format path — translate the GNU specifiers models type, reject the rest

`+FORMAT` is the hot path (filename stamps were 4/4 in the lite tier), so this is
higher-stakes than a footgun fix alone — and "reject everything unknown" is too
blunt for the one unknown specifier models actually reach for:

- **Translate `%N` (nanoseconds) to chrono** (`%9f` / `timestamp_subsec_nanos`), so
  `date +%s%N` *works* rather than panicking or erroring. Two models typed `+%s%N`;
  it's real GNU, and the translation is a few lines. (Add `%:z` likewise if a probe
  shows demand.)
- **Reject genuinely-unknown specifiers loudly** — validate the format string up
  front (or catch chrono's `Item::Error` before `.to_string()`), returning
  `exit 2: date: unknown format specifier '%Q'`. This is the "validated before
  execution" promise applied to the format mini-language, and it closes footgun #1:
  **no specifier, known or unknown, can panic the worker.**
- **`TZ` is honored or refused, never silently wrong.** Either thread an explicit
  timezone (read `TZ` from the exec env and resolve via `chrono-tz`, or add
  `--tz=ZONE`), or — if we won't support per-call zones yet — detect a `TZ=`
  assignment in scope and refuse with a clear message. Honoring is better
  (`TZ=zone date` was typed in both tiers); refusing loudly is the acceptable
  interim. Closes footgun #3.

### 4. `-d` / `--date STRING` — the hard part, the high-value part

The one feature that's real work, and the one the whole fleet reaches for first.
Target the empirical subset, not all of GNU:

- `yesterday` / `tomorrow` / `now` / `today`.
- `N {sec,min,hour,day,week,month,year}[s] ago` and `+N units` / `N units`.
- `next/last <weekday>`.
- An absolute ISO date/datetime (`2026-01-01`, `2026-01-01T09:00`), so
  `date -d 2026-01-01 +%s` (Sonnet typed this) works.
- **An absolute date with a trailing offset** — `2026-06-01 -1 day`. Models nest
  substitutions and feed the result straight to `-d`: DeepSeek V4-flash produced
  `date -d "$(date +%Y-%m-01) -1 day"` (last-day-of-previous-month). The grammar
  must accept `<absolute> ± <offset>`, not just one or the other in isolation.
- `@N` is already handled by §1; `-d "@N"` routes into the same decoder.

**Build vs. crate.** chrono won't parse "next friday." Options:
- *Hand-roll the empirical subset* (~a day): a small recursive parser over the
  forms above. Total control, no new dep, fails loud on anything outside the subset
  — which is the kaish-correct behavior (don't half-support a sprawling grammar).
- *Pull a crate* (`interim`/`chrono-english`, pure-Rust): more coverage for less
  code, at the cost of a dep whose grammar we don't control and whose error
  behavior we'd have to audit against "fail loud."

Recommendation: **hand-roll the subset.** It matches "familiar syntax, fewer
footguns, validated before execution" better than inheriting an open-ended NL
grammar, and the experiment *gives us the exact subset to target* — we're not
guessing at coverage.

### 5. `-r FILE` — file mtime (justified by fit, not demand)

Demand is only moderate (3/9 — Opus, Gemini Pro, Gemma 31B), so this earns its place
on *fit*, not frequency: unlike the rest of GNU `date`, `-r` *reads the filesystem* —
squarely kaish's wheelhouse, and the one form that exercises the read-only VFS.
Genuinely useful to a read-only explorer ("when was this last touched?"). Resolve
`FILE` through the VFS (containment + read-only apply for free) and format its mtime
through the same path as `now`.

### 6. `--json` — a bet the survey can't back yet (validate before building)

Every kaish builtin can emit structured output, and for a *model* consumer a
`date --json` that hands back every field at once *looks* like the most ergonomic
addition of all — pull `.weekday` instead of recalling `+%A`:

```
date --json
{"iso":"2026-06-14T09:03:15-04:00","epoch":1781442195,"utc":"2026-06-14T13:03:15Z",
 "local":"2026-06-14 09:03:15","weekday":"Sunday","tz":"-04:00","offset_seconds":-14400}
```

But be honest about the evidence: **0 of 9 models reached for anything JSON-shaped.**
The survey measures what models type unprompted, and it says they think in strftime
and `-d`, not JSON — so this is a designer's hypothesis, not an observed behavior.
Cheap way to test it first: a follow-up probe that *offers* a model a `--json`-capable
`date` and sees whether it picks the field over the format string. Build it if the
probe shows uptake; until then it's surface area nobody asked for.

---

## Determinism note

`date` reads the wall clock — it's the one builtin whose output isn't a pure
function of the VFS, so it injects nondeterminism into otherwise-reproducible read
sessions. That's benign (reading the clock mutates nothing, and the read-only
invariant is untouched), but it argues for an **injectable clock** behind the
builtin: tests currently assert loose properties ("contains a `-`", "all digits,
> year 2000") *because they can't pin "now."* A `Clock` trait (real `Utc::now` in
prod, fixed instant in tests) lets the new `-d`/`-r`/`--json` paths be tested
exactly — e.g. `date -d "@1700000000" -u` must equal `2023-11-14T22:13:20Z`, a real
assertion that *can and will fail* if the epoch math regresses. (This also makes the
panic-on-`%N` test deterministic.)

## Scope discipline — what we deliberately won't do

- No `date -s` / setting the clock (kaish never mutates host state; it's read-only).
- No full GNU NL grammar ("3rd thursday of next month", "fortnight"). Cover the
  experiment's subset; fail anything else loudly.
- No locale-aware month/day name tables beyond what chrono gives for free.
- No BSD `-v`/`-j -f` surface — we pick the GNU dialect the fleet expects and say so
  in the help, rather than straddling both and matching neither. This is now
  data-backed, not a judgment call: **9/9 models were GNU-shaped, the tiny ones
  included** — there is zero measured operator demand for the BSD dialect.

## Test plan (TDD)

Per the project standard — tests that can and will fail:

1. **Footgun regressions first (red):** `date "@1700000000" -u` equals the known UTC
   string `2023-11-14T22:13:20Z` (today it echoes); a genuinely-unknown specifier
   like `date +%Q` returns `exit 2` (today *any* unknown specifier panics); `date
   +%s%N` *produces nanoseconds* per §3 (today it panics); `TZ=Asia/Tokyo date -u
   +%H` and a Tokyo-zone assertion diverge correctly (today they don't).
2. **The survey is the golden corpus.** The assertion set = the **union of all nine
   models' lists**, checked in as a fixture. Every entry must either produce the
   right answer or fail loud — that turns "what models actually send" from prose in
   this doc into an enforced contract, and it's the artifact that catches the next
   regression. Run each against the injectable fixed clock.
3. **`-d` grammar:** one assertion per subset form in §4, *including* the
   absolute-plus-offset case (`date -d "2026-06-01 -1 day"` = `2026-05-31`) that the
   nested-substitution idiom needs.
4. **`--json` shape (only if §6's probe greenlights it):** parse the object, assert
   `epoch` ↔ `iso` consistency at the fixed instant.

## Effort estimate

- `@N` epoch-decode (§1): **an afternoon** — the highest-value slice, shippable on
  its own ahead of everything else, and it closes a silent footgun.
- Aliases (§2), format-path hardening + `%N` translation (§3), `-r FILE` (§5),
  injectable clock: **~half a day**, all mechanical, chrono does the formatting.
- `-d`/`--date` hand-rolled relative parser over the empirical subset (§4): **~a
  day**, most of it tests.
- `--json` (§6): gated on a probe — don't budget it until the demand is shown.
- The skeleton itself is free — the `Tool` trait is three methods and the file is
  already ~200 lines including tests.

The headline: *the builtin was never the hard part.* The hard part is one small
natural-language parser, and the experiment handed us its exact spec.
