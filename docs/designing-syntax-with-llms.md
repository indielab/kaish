# Designing a Language by Asking the Language Models

*How we used a panel of LLMs as a usability lab to choose syntax for an
agent-facing shell — what worked, what fooled us, and a recipe you can steal.*

---

## The premise

[kaish](https://github.com/tobert/kaish) is a shell whose primary users are AI
agents. It's a `sh`-flavored language an LLM drives as an MCP tool: pre-validated,
no word splitting, structured output. When the main author of the code you're
designing is a language model, a question follows that feels slightly heretical:

> If your users are language models, then language models are your usability lab.

Human taste still matters — readability, consistency, the feel of the thing. But
"can a model reliably *emit* this syntax under realistic conditions" stops being a
guess you argue about and becomes a thing you can **measure**, today, for the price
of a few API calls. So we did. This is the writeup of the method, grounded in a
real decision: adding array/hash (list/record) literals to kaish. The design doc
that fell out of it has since been retired into this one — what was worth keeping
was never the decision log but the notes on how to *teach* the syntax we chose,
and those are the [teaching section](#teaching-it-the-docs-are-part-of-the-design)
near the end. This doc is about *how* we got there.

The short version: a few hours of `oneshot` calls to DeepSeek, Gemini, and Claude
Haiku settled a dozen syntax questions, caught problems no amount of armchair
debate would have, and — the punchline — taught us that the model picking the
syntax matters far less than *how completely you teach it*.

---

## The setup

Nothing fancy. The instruments:

- **Cross-vendor breadth.** DeepSeek V4, Gemini 3.x, Claude Haiku. Different
  training mixes fail differently; agreement across vendors is a much stronger
  signal than one model nodding along.
- **Capability tiers, deliberately.** Not just the flagship models — the *small,
  fast, fast-and-loose* ones (DeepSeek-flash with thinking off, Gemini-lite,
  Haiku). These are the MVPs of the whole exercise. A capable model papers over a
  shaky design; a cheap one face-plants on it and shows you exactly where the
  cracks are.
- **Stateless one-shots.** Each test is a self-contained prompt: a tight syntax
  cheat-sheet plus a handful of tasks, "output code only, no prose." No
  conversation state to contaminate the result. Cheap, parallel, seconds per run.

That's it. The leverage isn't the tooling, it's the loop.

**A note on the cheap and local models,** since they're the MVPs here and the
most finicky to run. Reasoning models return the answer in a separate
`reasoning_content` channel — with a small output budget a verbose one hits the
token cap mid-thought and hands back an *empty* `content`, scoring zero on a
correct answer. Give them a large `max_tokens` and server context, or grade the
reasoning trace directly. Forcing `enable_thinking: false` for brevity trades
accuracy for speed — the reasoning is doing real work, and amputating it drops
sigils and operators. And "local" is gated by what actually fits in VRAM beside
its KV cache, not by the spec sheet; probe `/v1/models` across listening ports
rather than trusting documented ones.

---

## The loop

1. **Draft a candidate syntax** as a terse cheat-sheet — the kind of thing you'd
   put in the real docs.
2. **Write bash-tempting tasks.** Pick tasks where a model's training priors
   (bash, Python, JS) would *leak* if your syntax is weak. "Iterate a hash and
   print each key/value." "Append in a loop." "Check membership and branch." If
   the model reverts to `${arr[@]}` or `declare -A`, you'll see it.
3. **Grade the divergences, not just the pass/fail.** This is the core trick. When
   two models disagree, or one fumbles, that spot is almost always **an
   under-specified corner of your design**, not a dumb model. The errors are a map
   of your spec's holes.
4. **Iterate.** Tighten the ambiguous corner, A/B a real alternative, vary how
   much you spell out, and re-run.

The output you care about isn't "did it score 9/10." It's *where* the 1/10 landed
and *why*.

---

## The heuristics (the reusable part)

These generalize past kaish. If you ever design syntax, config, or a DSL that an
LLM will write, these are the lessons that earned their keep.

### 1. A divergence is a spec gap wearing a model costume

Our first round on membership offered a `has` command: `has $list elem`. The
flagship models handled it. The fast ones did not — and they failed *differently*:

```sh
# deepseek-flash wrapped it as a value:
if $(has $colors green) { ... }
# gemini-lite garbled the arguments entirely:
if has keys $inventory "bananas" { ... }
```

Two models, two different wrong answers, same root cause: a bare command named
`has` is ambiguous about whether it's a value or a control-flow predicate, and the
models papered over the ambiguity in incompatible ways. The divergence *was* the
finding. We re-spelled it as a test operator —

```sh
if [[ green in $colors ]]; then echo "has green"; fi
```

— and both fast models went 7/7 across key-membership, `not in`, nested lists,
membership-in-a-loop, and compound `&&`. `in` slots into an existing mental model
(`==`, `-f`, Python's `in`, `for x in`); a bare command doesn't compose. We didn't
"fix the models." We fixed the spec the divergence pointed at.

### 2. The weak-model tail is your actual constraint

Run the same test across tiers and you'll watch a design's true robustness
separate from a model's raw strength. Our nest-vs-spread rule — a bare `$xs` inside
a list literal *nests* as one element, `...$xs` *flattens* it —

```sh
nested=[$a $b]        # a list of two lists
flat=[...$a ...$b]    # one flat list
```

— went **12/12 on both lite models**, with perfect discrimination in both
directions and unprompted nested indexing. That's a green light you can *trust*,
because it held at the bottom of the capability range. Conversely, a design that
only the flagship gets right is a design that will generate broken code in
production the first time someone routes through a cheaper model. **Design for the
tail.** The flagships are fine either way (more on that below).

### 3. What you don't show, the model fills from its priors

Models are relentlessly associative. Anything your spec leaves unspecified gets
back-filled from training — and for a shell, training means *bash*. Some receipts:

- Omit an append idiom, and every model reaches for `xs=[$xs new]` splat —
  bash/Python muscle memory.
- Omit list indexing from an example, and a model invents `$(colors.1)` —
  dropping the `$`, hallucinating a `$()`.
- Omit how to init an empty list, and Haiku — *Haiku!* — leaks bash array syntax
  `keys_list=()`.

The corollary is uncomfortable but useful: **silence is not neutral.** If you
don't want the bash default, you have to actively show the alternative, including
a "don't do this" counter-example, because the model's hands type the prior.

### 4. Teach an operator inside its full control structure, never bare

This one cost us a near-miss. A terse cheat-sheet listed membership as a standalone
line:

```
[[ key in $r ]]    # membership
```

A model read that as a *complete statement* and emitted malformed garbage when
asked to branch on it (`if then; do … fi`). The *same model*, shown the operator
inside a full `if [[ … ]]; then … fi`, produced flawless code. A novel operator
demonstrated in isolation reads as a finished thought. Always document it embedded
in the construct you actually want.

### 5. Vary the scaffolding to separate *compliance* from *default pull*

A spec sitting directly above the tasks with an explicit "don't use bash" is an
easy exam — it measures whether the model can *follow* your rules, not whether it
*reaches* for them. So vary the conditions:

- **Rules-only** vs **example-only** (no rules, just a worked snippet to imitate).
  These fail differently — example-only nails exactly what the example shows and
  guesses wrong on what it omits.
- **Drop the anti-bash warning** and see what leaks when not suppressed.
- **Bury the spec** behind filler to test recall vs recency.
- **Omit the idiom entirely** and watch what the model invents — that tells you the
  default you're fighting (see #3).

Each framing is a different lens on the same design. The example-driven runs matter
most, because that's how your real docs will teach.

### 6. A/B the *real* alternatives, head to head

Don't just validate your favorite — pit it against the contender on identical
tasks. `has` command vs `[[ in ]]` operator. Bare `$xs[0]` access vs braced
`${xs[0]}`. 1-indexed vs 0-indexed. The relative error rate between two concrete
syntaxes is worth more than an absolute score on one.

### 7. Ground it against the actual implementation

Model ergonomics is one axis; parser cost is the other, and they trade off. Before
committing the access syntax we read kaish's real lexer/parser/validator. It turned
out braced `${path}` access *rode infrastructure that already existed*, while bare
`$user.name` postfix access needed a whole new grammar — and braced is
bash-consistent (`${arr[0]}`) besides. The model evidence and the implementation
cost pointed the same way; that's a decision you can make with both hands. Letting
models vote on a syntax your parser can't cheaply support is how you design
yourself into a corner.

---

## A few war stories

**Membership** was the cleanest win — a `has` *command* the small models couldn't
keep straight became `[[ key in $r ]]` and went 7/7 on the tail. (#1)

**The bare for-head.** `for k in keys $r; do …` — letting a builtin sit unwrapped in
the loop head — is technically a parser special case, but *every* model wrote it
naturally and none reached for `$(keys $r)`. When the entire panel converges on the
ergonomic form unprompted, fighting it for purity's sake is a tax you pay forever.

**The silent-corruption catch.** Under reduced scaffolding, a capable model wrote
`append $colors purple`, **threw the returned list away**, and then reported the old
length — a silently wrong answer. That observation killed the pure-functional
`append` and made in-place `push` the primary idiom. A usability test surfaced a
*data-integrity* hazard, not just an ergonomic one.

**A second model as design reviewer.** We handed the whole draft design doc to
Gemini Pro and asked for a hostile review. It earned its keep: it killed our
overload of `$()` for string interpolation (use the `${…}` that already bounds
expansions), caught that our new `${#xs}` length collided with the existing
`${#NAME}`, flagged that `export`-ing a structured value was undefined behavior, and
spotted that we allowed commas in records but not lists — a guaranteed model
stumble. Different model, different role (critic, not generator), high yield.

---

## The two ways the method fooled us

Honesty is part of the method, so:

**We tested a language we don't ship.** Several early cheat-sheets used curly-brace
blocks — `for x in xs { … }` — which kaish *doesn't have*; its blocks are sh-style
`do/done` / `then/fi`. The models happily used whatever delimiter we showed, so the
mistake rode along invisibly until someone re-read the actual language reference.
The collection-specific findings survived (they don't depend on the block
delimiter), but the lesson stands in neon: **test the real target syntax, or you're
validating a dialect that doesn't exist.** Pin your harness to the shipping grammar.

**Easy-condition bias.** Our first runs had the spec directly above the tasks *and*
an explicit "no bash." That measures compliance, not instinct, and it flatters your
design. We only learned what the syntax's *default pull* was after we stripped the
warning and the worked examples (see #5). If your test conditions are too kind, your
green checkmarks are lying to you.

---

## The meta-finding: calibrate to the tail, not the flagship

The most decision-relevant result came last. We fed Haiku the *discarded* syntaxes —
bare access, the `len` builtin, the nested `len $(keys $r)`, the `has` command in an
`if`, 1-indexing, implicit splat — fully expecting it to struggle.

It went 8/8 on essentially all of them. It followed 1-indexing without reverting. It
used `has`-in-`if` correctly — the exact thing that broke the smaller models. Its
*only* stumble was a spec gap (no empty-list example → it leaked bash `()`).

So the capable model is robust to almost any syntax you hand it. Which means the
syntax choice was never *for* the capable model. It was for:

1. the **weak-model tail** that actually fumbles,
2. **consistency** (one length form, one append idiom),
3. **parser/implementation cost**,
4. **silent-failure traps** (the discarded-`append` data corruption),

and — above all — **completeness of how you teach it**, since even Haiku falls back
to bash exactly where the spec goes quiet. Design and document for the weak tail and
for completeness. The strong models will be fine. They were always going to be fine.

---

## Teaching it: the docs are part of the design

That corollary cost us more effort than the syntax decisions did. If completeness
of teaching is the variable that actually moves the numbers, then the doc copy
isn't downstream of the design — it *is* part of it, and it earns the same lab.
What follows came out of grading model output against draft teaching copy: first
ad-hoc cheat-sheets, and at the end the real shipped help text.

**Show it. Stating it is not enough.** We had a rules sheet that plainly stated
paths must be braced inside strings. Models read it and emitted unwrapped paths
anyway. An *example* that showed `"${r[$k]}"` got copied verbatim, every time.
Models reproduce what's on the page and guess at what isn't, so we ended up
listing every access form side by side — `${xs[0]}`, `${xs[-1]}`, `${xs[0:2]}`,
`${r[k]}`, `${r[$key]}`, `${r["weird key"]}`, `${r[a][b]}` — after example-only
teaching that omitted list indexing produced an invented `$(colors.1)`, dropped
sigil and hallucinated `$()` included.

**Show the wrong form with its error, right next to the right one.** The bash/JS
prior for field access is a dot, so the docs show `${user.name}` as the WRONG
form together with the error it produces. That's not a stylistic nicety; it's the
only version that worked. The payoff was measurable at the end: on a bare
field-access task with no "don't use dots" warning anywhere in the prompt, the
panel produced *zero* dot-leakage. A taught contrast holds where a stated rule
evaporates.

**Adjacency does work that prose can't.** Two rules kept getting smeared into one:
access and length are expansions and take no `$()` (`${xs[0]}`, `${#xs}`), while a
builtin used as a value needs it (`$(keys $r)`). Explained in separate paragraphs,
models overgeneralized the capture rule across both. Printed next to each other,
the boundary held. Same story for nest-vs-spread — `[$a $b]` beside
`[...$a ...$b]` went 12/12 on the lite models, in both directions.

**Anchor a novel form to one the reader already has.** `push colors cyan` takes a
variable *name*, not a value — an odd calling convention that models got right
every single time. We think the reason is one clause of prose: it works like
`read`. A sentence of analogy to something already in the language buys more than
several sentences of specification.

**One operation, one spelling.** We dropped the pure-functional `append` in favor
of in-place `push`, then made sure no document showed both. A model that has to
choose between two spellings of the same operation will sometimes choose the one
you were about to remove.

**Teach the boring form too, or the novel one over-attracts.** Given no `!=`
example, Haiku expressed "not equal to dog" as `[[ $a not in [dog] ]]` — a
list-membership test standing in for scalar inequality. Correct, and utterly
roundabout. A shiny new operator will absorb work that belongs to the plain one
unless the plain one is on the page. It also forced a design consequence: `in`'s
right-hand side had to accept a literal and not just a variable, because models
will write one.

**Error messages are teaching copy, and the highest-leverage kind.** Models
context-switch out of JSON and Python and write `x = [a, b]`. A validator that
merely rejects that buys you a retry; one that shows the fix — "kaish assignment
takes no spaces around `=`; write `x=[a, b]`" — converges the model in a single
round. So every loud error the collections work added was written that way:
dotted access says *use `${user[name]}`*, a multi-word literal says *quote it*, an
out-of-bounds set says *`push` grows lists*. An error message is the one piece of
documentation you can guarantee the model reads, at the exact moment it is
confused.

**Turning the ambiguous form into an error relocates the trap; it doesn't remove
it.** A hostile review caught that `for x in $data` would silently iterate a
*record's keys* when an API returned an object where a list was expected — a
silent type cascade, the worst class of bug in a shell agents drive. So we made
the bare form a hard error. The trap promptly moved into the sanctioned idiom:
`for x in $(values $data)` on a record iterates its field values instead of the
one object, just as silently. The real fix was a shape guard (`typeof`,
`[[ -list ]]`, `[[ -record ]]`) plus docs that show the guard wherever the shape
of the data isn't trusted. When you plug a hole, go look where the water comes
out.

**The teaching copy has a budget, and the tier most likely to be read is the
scarcest.** The always-on instruction block an embedder ships was already
~9–10K characters, dominated by the builtin index. The dense teaching above
cannot live there. So collections got a handful of terse rules and exactly one
wrong-form contrast in the always-on tier, the full tested example set in the
reference tier (`help syntax`), and the prose in the language reference. Amy's
framing for the always-on block, which we adopted: lead with the most important
200–300 characters and descend in ranked steps, so a skimmed or truncated read
still delivers the rules that matter most, with everything below it reachable by
name.

**Test the artifact you ship, not the cheat-sheet you wrote for the test.** Every
round up to the last handed the panel an ad-hoc cheat-sheet — convenient, and one
step removed from what an agent actually receives. For the final pass we made the
panel's *entire* reference the real composed help output: the shipped
`Recipe::agent_onboarding()` block plus the shipped collections help section, byte
for byte, no repo access, no other context. DeepSeek V4, Gemini 3.5-flash and
Claude Haiku 4.5, stateless one-shots, six tasks each covering every form that had
changed since the previous panel.

Eighteen for eighteen, no correction rounds. All three models wrote a nested record
literal and iterated it as
`for k in $(keys $servers); do echo "$k: ${servers[$k][port]}"; done` on the first
try — the exact construction the earlier panel most often got wrong, now reached
for unprompted. Dynamic subscripts came out distinct from literal keys, membership
arrived wrapped in a full `if … then … else … fi`, the slice was right, dot-leakage
was zero. All eighteen generated scripts were then executed against the real `kaish`
binary and produced correct output.

The argument for testing the shipped copy, though, landed *before* the panel ran.
Preparing the material turned up two shipped help fragments that taught membership
as a bare standalone `[[ k in $r ]]` line — precisely the failure mode we had
diagnosed rounds earlier and written a rule about (#4). It shipped alongside the
membership feature a month prior and survived every review since, because nothing
had ever pointed a panel at the composed artifact. We fixed the fragments and the
matching language-reference examples before the run. A rule you have learned is not
a rule your docs follow; only the artifact can tell you which.

**Pre-register the number and the response.** Before that panel we wrote down which
result would change the design: if models needed more than one round to accept the
mandated `$(keys $r)` loop head, we would ship a relaxation letting a bare
collection iterate. Three of three converged in round one, so we didn't ship it, and
that restraint is only credible because the threshold was set beforehand. Deciding
in advance what the number means is what keeps a green run from being read as
permission to do the thing you already wanted to do.

The obvious next move — which we have not done — is to run the doc copy the way we
ran the syntax: variants of the teaching text, measured against task success, across
model families and sizes. Same lab, different subject.

---

## The other variant: tuning an existing tool

Everything above is about inventing *new* syntax. The same lab runs in reverse —
when the tool already exists (`sed`, `awk`, `date`) and the question isn't "what
should this look like" but "does kaish do what an agent expects when it reaches
for the tool by reflex." A few things change, and the one thing you'd assume is a
free oracle turns out to be a trap.

**The panel becomes the spec, not the jury.** For novel syntax you grade whether a
model can *emit* your design. For an existing tool you survey, cold, what the fleet
*reaches for*: ask nine models to write `date` or `awk` one-liners with no docs and
no priming, and the convergence is the specification. When 9/9 reach for
`date -d "2 weeks ago"` or 3/3 reach for `awk -F:`, that agreement *is* the
definition of correct — the thing kaish must do consistently. Divergence among the
models maps the genuinely ambiguous corners; unanimous convergence is a behavior
you must either support or loudly refuse.

**Sort every gap into FIX / ADD / TEACH.** A claimed feature that misbehaves is a
FIX. A form the panel reaches for unprompted that kaish lacks is an ADD. A form
outside the 80% slice is a TEACH — error loudly with a hint, never a silent no-op.
The hazard ranking is the usual kaish posture, sharpened: *silent* wrong (wrong
answer, exit 0) is the enemy; a loud "not supported" is fine. The reflex forms land
squarely on the silent paths — both lite models reached for `sed`'s `;` separator
and `awk`'s `gsub`, both silently wrong before we fixed them. A mechanical
differential sweep catches these precisely because it can't look away: every output
mismatch is a finding, where a hand-written suite quietly omits the case that's
broken.

**A reference implementation is a sanity check, not the oracle — and here's the
trap.** It's tempting to diff kaish against `gawk` or GNU `coreutils` and call
parity "correct." Don't promote the reference to oracle. The `date` survey is the
cautionary tale: *every* model, flagship down to 4B-active, assumed GNU/Linux
(`date -d`, `%N`, flags that break on macOS/BSD) and **not one corrected for it
unprompted.** One even confessed the bias in its reasoning trace — "I'll focus on
GNU, that's most common" — and then didn't correct for it anyway. The monoculture
isn't a small-model artifact or a flagship artifact; *it's the weights, top to
bottom.* So the cross-model consensus you're treating as ground truth can be a
**shared training bias** wearing the costume of agreement. That's usable — it even
tells you which dialect to *be* (kaish chose to be GNU-shaped because the agents
that drive it are too) — as long as you name it. Use the reference impl
defensively ("did we introduce a silent divergence from what the fleet expects?"),
never prescriptively ("the reference is right, match it").

**These studies are disposable, by design.** The per-tool surveys that produced
this (sed, awk, date) were always meant to be ephemeral — a snapshot of what
*today's* models reach for. Re-run them as models advance; the convergence will
drift, and the GNU monoculture may not hold forever. What's durable is the method,
not the verdicts. That's why this is the one doc that survives and the per-tool
writeups don't.

---

## The recipe (steal this)

A checklist for using an LLM panel as a syntax usability lab:

1. **Write the cheat-sheet** you'd ship, in the *real* target grammar. Don't
   improvise delimiters.
2. **Pick bash-/Python-tempting tasks** — ones where training priors leak if the
   design is weak.
3. **Run a cross-vendor panel across capability tiers.** Weight the cheap,
   fast-and-loose models heaviest.
4. **Grade divergences, not scores.** Each disagreement or fumble is a coordinate
   on your spec's holes.
5. **Vary the scaffolding** — rules-only, example-only, no-warning, omit-the-idiom
   — to separate "can follow" from "will reach for."
6. **A/B real alternatives** head-to-head on identical tasks.
7. **Cross-check against your parser** so ergonomics and implementation cost vote
   together.
8. **Use a second model as a hostile reviewer** of the whole design.
9. **Re-test the final, changed forms** before you commit — the things you adopted
   late were never actually validated in their final shape — and re-test them
   against the **copy you actually ship**, not a cheat-sheet written for the test.
   Say in advance which result would change the design.

Total cost for the kaish collections design: a few dozen one-shot calls, a few
hours. It settled a dozen contentious syntax decisions with evidence instead of
vibes, caught a silent-data-corruption footgun and a half-dozen ambiguities, and
left a paper trail of *why* each call was made.

---

## What this does NOT tell you

Be honest about the method's blind spots:

- It measures **generation**, not **comprehension** or long-horizon use. A model
  emitting `${xs[0]}` once says nothing about debugging it three turns later.
- It's biased toward **today's** training priors. Re-weighting on the bash default
  is a moving target as models change.
- A **unanimous** panel can be unanimously *wrong* in the same direction. Shared
  monoculture bias (every model assumed GNU/Linux for `date`) reads as consensus
  but is really just the training distribution agreeing with itself. Agreement is
  evidence, not proof — name the bias, don't launder it into truth.
- It can't price **human** readability, long-term maintenance, or how the syntax
  composes with the *rest* of the language. Models judge the local form, not the
  global grammar.
- Green across an easy condition is not green. Mind the framing (the bias above).

It's a usability lab, not an oracle. But it's a *fast, cheap, repeatable* usability
lab with thousands of synthetic users who think a lot like your real ones — and for
a language whose users are language models, that's about as close to the real thing
as design feedback gets.

---

*Methodology notes from the kaish project. The collection-syntax design doc these
produced has been retired into this one; its teaching notes live above. Panel:
DeepSeek V4 (pro/flash), Gemini 3.x (pro/lite/3.5-flash), Claude Haiku (4.5) —
June–July 2026.*
