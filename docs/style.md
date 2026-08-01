# kaish writing style

kaish is a strict subset of `sh`, chosen so muscle memory transfers. This guide is a
strict subset of English, chosen for the same reason. A reader who understands the
language already understands the prose.

These are weights, not gates. There is no linter and there is no compliance pass. Apply
them when you write, and groom the text you touch.

Inspired by the structure of ASD-STE100 Simplified Technical English — a small constraint
set plus a project term list — but not STE and not claiming to be. The STE approved-word
dictionary is copyrighted and aerospace-shaped, so we keep our own.

## Where the weights apply

| Weight | Files |
|---|---|
| Full | `crates/kaish-help/content/en/`, fragment bodies in `crates/kaish-help/src/fragments.rs`, and every builtin `description`, `about`, example label, and `///` argument doc |
| Partial (terms and boundary; relax the rest) | `docs/LANGUAGE.md`, `docs/EMBEDDING.md`, `docs/NAMING.md` |
| Terms only | `README.md` and the design docs under `docs/` |
| Exempt | `docs/devlog.md`, `signoff.md`, `docs/designing-syntax-with-llms.md`, `docs/arrays-and-hashes.md` |

Exempt text tells a story from a point of view, and a story needs a voice.

kaibo and kaish-extras adopt this guide by reference as they evolve. kaijutsu is exempt.

## The weights

### Subset, not dialect

Keep the vocabulary small. This is a constraint on how many **distinct** words the corpus
uses, not on how long the text is — a smaller vocabulary usually costs words, and that is
the correct trade.

The class to avoid is the metaphor that names a mental act as a physical one: "reach for,"
"the defensive-quoting dance," "escape hatch." A reader who learned English second, or a
model working from a partial context, cannot recover the intent from the figure.

Some idiom is load-bearing and stays. `muscle memory` names the design thesis in two
words. `footgun` names a hazard class we ship a fix for. Treat these as terms, not as
decoration, and do not add more.

> Before: Reach for `test` for muscle memory or where a plain command is wanted.
>
> After: Use `test` when you want a plain command, or when the `sh` habit is faster to type.

### One term, one meaning

Pick one word for each concept and keep it. Do not vary the word for style — a synonym
reads as a new concept, and the reader spends attention deciding whether it is one.

Terms that carry a guarantee live in the table in `CLAUDE.md`, which is the source.
`README.md` mirrors it for readers who never open `CLAUDE.md`; keep the two in step. This
guide does not copy it — three copies drifted within a day of being written.

### State the number

Agents act on our numbers. Give the exact exit code, the exact size, the exact flag, and
the exact default. A vague verb is a defect in this corpus.

> Before: Oversize output fails.
>
> After: Oversize output spills to a file and exits 3.

State the default and the condition too: "reads stdin when no files are given," "off by
default; applies to `-r` only."

### Fail loud

Put the constraint and its consequence at the front of the sentence. Do not bury a hazard
in a subordinate clause, and do not soften it with a hedge. This mirrors what the shell
itself promises: the boundary is loud.

The first sentence must also work alone. The always-on onboarding spine is capped at 3500
characters (`compose.rs`, `onboarding_spine_stays_within_budget`) and readers skim, so
write so that a truncated fragment still carries the rule.

> Before: Note that files removed this way may not be recoverable in some configurations.
>
> After: `rm` deletes the file permanently unless `set -o trash` is active. Turn on `trash`
> first if you want a recoverable copy.

### Keep the why

A rule earns its rationale. The house pattern is `<rule> — <why>`, and the clause after
the dash is load-bearing: a reader who knows why can guess correctly at the edges, and a
reader who has only the rule cannot.

When a sentence gets tangled, split it. Never drop the rationale to fit. There is no word
budget — counting words instead of judging the sentence is how this weight goes wrong.

**When the source records no rationale, leave the rule bare.** Do not invent one. A bare
rule next to an explained one is honest, and it marks where a real answer is missing.

**Contrast is a rationale.** Comparing against bash is one of the most effective moves in
this corpus, and it is endorsed: "Bash splits unquoted `$VAR` on `$IFS`; kaish never does."

**Tables carry the same weights.** A table cell is prose with the subject moved into the
column header. Write cells as complete clauses — a fragment forces the reader to
reconstruct the verb, and a model reading one cell out of context cannot. Put the rule in
the cell and the rationale after a dash. Expect a table rewritten this way to get longer.
That is the correct trade.

### Do not leak the kernel

Reader-facing text describes what the reader must predict. The test is not whether a
sentence names an internal — it is whether the reader needs that internal to predict
behavior. `[[ ]]` lexes as two bracket tokens is a mechanism *and* the whole contract for
why `[ -f x ]` fails, so it stays. `to_argv()` joins the pair is neither, so it goes.

The boundary has a precise location in the builtins. A `///` comment on an **argument** is
published: `params_from_clap` copies it into `ParamSchema.description`, the kernel exposes
it through `Kernel::tool_schemas()`, and the embedder ships it to the model. A `///` on the
**struct** is never published — `schema_from_clap` reads `cmd.get_about()` instead — so
struct docs and `//` comments are both safe places for mechanism.

> Before: `/// Unset a variable (-u VAR). Repeatable: -u A -u B. Clap sees a single`
> `/// occurrence via to_argv() ... This field is a validation sink only.`
>
> After: `/// Unset a variable (-u VAR). Repeatable: -u A -u B.`

**When you touch a builtin, audit every `///` on its clap struct.** Grooming alone cannot
reach this class: the mechanism leaks sit in files nobody has reason to open, so the audit
has to ride along with any visit to the file.

### Groom at the point of touch

When you edit a file, bring the part you edited into voice. Leave the rest alone.

We are not scheduling a rewrite. A bulk pass would freeze this guide before we know
whether it works, and it would separate the style decision from the person who understands
the text. Grooming keeps both together.

## Known debt

These are real violations, found by cross-model review of this guide. They are recorded so
that whoever next touches these files knows to fix them, not as a rewrite plan.

- Roughly ten builtins publish `/// Sink — to_argv() always emits -- before positionals`
  to the model (`pwd.rs`, `vars.rs`, `hostname.rs`, `kaish_clear.rs`, `kaish_last.rs`,
  `kaish_status.rs`, `kaish_version.rs`, `true_false.rs`, and others).
- `jq_native.rs` publishes `consumes=2` and clap-layer mechanism; `--argjson` says
  `See _arg above`, naming a private field.
- `sed.rs` publishes `(clap Append → schema repeatable)`.
- Example labels vary in voice: imperative ("Print a field"), noun phrase
  ("Case-insensitive search"), and mechanism ("Alternation (ERE or GNU BRE)"). We have not
  picked one.
- Cross-references take three forms in full-weight text: ``see `help syntax` → Collections``,
  ``see `help fromjsonl` ``, and `see "Not supported" below`. We have not picked one.
