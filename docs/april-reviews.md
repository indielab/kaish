# Code Review: kaish-kernel (April 2026)

## Overview
A thorough review of the `kaish` kernel was conducted, focusing on correctness, failure modes, and architectural integrity. The system is well-structured and follows a "structured data first" philosophy.

**Validation pass (2026-04-10):** Each finding below was re-checked against the current source. Status tags reflect that audit; citations are file:line at the time of validation.

## Key Findings

### 1. Concurrency & Data Races — **RESOLVED (2026-04-10)**

The `Kernel` struct is auto-`Send + Sync` but was **not safe under concurrent `execute()` calls on the same instance**.

- **State Clobbering — confirmed:** `dispatch_command` (`kernel.rs:3114-3150`) and `execute_pipeline` (`kernel.rs:1332-1408`) both snapshot scope/cwd/aliases out of the kernel's `RwLock`s into a local `ExecContext`, run, and then write the mutated state back. Two concurrent `execute()` calls can each take a snapshot, run, and clobber each other on write-back. Lost-update, not memory-unsafety.
- **Shared Stderr — confirmed:** One `tokio::sync::Mutex<StderrReceiver>` per kernel (`kernel.rs:461`), drained at `kernel.rs:845, 2325, 2382`. Concurrent `execute()` calls would race on `drain_lossy()` and one task can swallow another's stderr.
- **Was mitigated only by call sites:** The MCP server creates a fresh `Kernel` per request (`crates/kaish-mcp/src/server/execute.rs:206`), and the REPL is single-execute-at-a-time. The footgun affected embedders that kept a kernel around and called `execute()` from multiple tasks.

#### Resolution

Two complementary changes in `kaish-kernel`:

1. **Per-kernel `execute_lock`** serialises concurrent foreground `execute()` calls.
   - Field: `execute_lock: tokio::sync::Mutex<()>` on `Kernel`. Tokio's mutex is fair (FIFO) and *is* the queue.
   - Acquisition happens in `Kernel::execute_streaming` (public), which then delegates to a private `execute_streaming_inner`. `Kernel::execute` flows through `execute_streaming`, so the lock covers both entry points without any self-deadlock risk.
   - On contention, `Kernel::acquire_execute_lock` emits `tracing::warn!(target: "kaish::kernel::concurrency", …)` — silent serialisation is observable in logs, matching kaish's "no silent fallbacks" rule.
   - `execute_streaming`'s callback trait bound was relaxed to `+ Send` (`&mut (dyn FnMut(&ExecResult) + Send)`) so that `execute()` futures are `Send` and can be used from `tokio::spawn` — required for any real concurrent usage and for the regression suite below.

2. **`Kernel::fork()` + deletion of `BackendDispatcher`** unlocks *true* parallelism for background jobs, scatter workers, and concurrent pipeline stages.
   - New inherent method `Kernel::fork(&self) -> Arc<Self>`: snapshots per-session state (scope via COW Arc, user_tools, exec_ctx), Arc-shares read-mostly resources (`tools`, `vfs`, `jobs`, optional `terminal_state`), and freshly constructs per-fork resources (`stderr_receiver`, `cancel_token`, `execute_lock`). The returned Arc is `into_arc`'d so the fork's `self_weak` points at itself — nested dispatch through `ctx.dispatcher` (e.g. inside `timeout`) routes through the fork, not the parent.
   - `CommandDispatcher` trait gained an `async fn fork(&self) -> Arc<dyn CommandDispatcher>` method; the `Kernel` impl delegates to the inherent `Kernel::fork` via UFCS.
   - `Kernel::dispatch_command` now (a) populates `ctx.dispatcher` from `self.dispatcher()` at the top of every call, ensuring forks dispatch through themselves, and (b) syncs the streaming pipe endpoints (`pipe_stdin`, `pipe_stdout`, `stderr`) in addition to scope/cwd/aliases, so tools running under a fork see the right I/O wiring.
   - `execute_background` (`kernel.rs:1462`) forks instead of constructing a `BackendDispatcher`; the spawned task dispatches through the fork and therefore has access to user-defined tools, `.kai` scripts, and `$(...)` in arguments — all of which silently failed before.
   - `scheduler::pipeline::run_scatter_gather` and `run_pipeline` each `dispatcher.fork().await` per concurrent worker/stage (a single shared fork would reintroduce the same clobber we're fixing).
   - `scheduler::scatter::ScatterGatherRunner` dropped its `parallel_dispatcher` field. `run_parallel` now forks per worker from the sequential dispatcher. Each worker has independent mutable state.
   - `BackendDispatcher` is gated `#[cfg(test)]` — production code has no uses. The struct, its trait impl, its external-command fallback, and related imports all live only in test builds. The public re-export in `kaish-kernel/src/lib.rs:59` was removed.

- **Regression suite:** `crates/kaish-kernel/tests/concurrency_tests.rs` contains nine tests, all running under `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`:
  1. `concurrent_cwd_no_clobber` / `concurrent_var_no_clobber` / `concurrent_alias_no_clobber` — 4–8 tasks × 50 iterations each mutate and observe scope/cwd/aliases; must never see another task's values.
  2. `concurrent_stderr_isolation` — 4 tasks each tag their stderr; each `ExecResult` must only contain its own tag.
  3. `background_job_does_not_block_foreground` — the lock releases after `&` returns; a foreground `echo` completes within 300 ms of a 400 ms background sleep.
  4. `background_job_runs_user_function` — a POSIX-function user tool runs inside `&`. Before the fork refactor this silently errored with "command not found".
  5. `background_job_snapshot_isolation` / `parent_does_not_see_background_mutation` — the background fork's scope is a snapshot at spawn time, and mutations inside the fork do not leak back to the parent.
  6. `scatter_parallel_runs_user_function` — scatter parallel workers run a user function (previously impossible).

  **Counter-factual verified:** temporarily commenting out the `execute_lock` acquisition reliably crashes tests 1–3 with `cannot pop the root scope frame` — the pre-existing `pop_frame()` panic at `scope.rs:103` is no longer reachable under the lock.

- **Bonus:** the fork refactor also **fixed six pre-existing scatter tests in `scheduler::pipeline::tests`** that had been broken since commit `1f80be6` (March 19) because they required `ctx.dispatcher` to be set on an `ExecContext` that never had it. Scatter now materialises its own sequential dispatcher by calling `dispatcher.fork().await` on the `&dyn` dispatcher already passed down by `PipelineRunner::run`.

- **Known non-regressions:** the `test_exec_builtin` libtest hangs because `std::os::unix::process::CommandExt::exec` replaces the test binary itself; four `tools::builtin::timeout::tests::*` tests panic on the lexer's rejection of numeric-prefix identifiers like `5s`; and `vfs::local::tests::test_symlink_absolute_target_escape_blocked` fails. All five failures are present on clean `main` (verified via a throwaway worktree) and are untouched by this work.

- **Help text follow-up:** `crates/kaish-kernel/docs/help/scatter.md:33` still contains a note that scatter workers "can only run builtins and external commands" — it is now out of date and should be removed/replaced in a separate sync-docs pass.

### 2. Word Splitting (Design Departure) — **VALID**
`kaish` explicitly avoids implicit word splitting.

- **Behavior — confirmed** (`kernel.rs:1016-1030`): the for-loop iterator special-cases JSON arrays (iterates elements) and otherwise treats the value as a single string. Inline comment: `"Strings are ONE value - no splitting! Use $(split "$VAR") for explicit splitting"`.
- **`split` builtin — confirmed** (`crates/kaish-kernel/src/tools/builtin/split.rs:160-168`): builds a `serde_json::Value::Array` and stores it on `result.data`, which is the field the for-loop reads.
- **Documented** in `docs/LANGUAGE.md:186-212`.
- **Note on the nominal "ExecResult.data" field:** `ExecResult` actually has *both* `data: Option<Value>` (parsed-JSON, public) and `output: Option<OutputData>` (structured render model, private). They are complementary; see `crates/kaish-types/src/result.rs:20-30`.

### 3. Arithmetic Evaluation — **VALID**
- `checked_add/sub/mul` at `arithmetic.rs:147-148, 153-154, 172-173`.
- Division/modulo by zero `bail!` at `arithmetic.rs:178-179, 187-188`.
- Recursive descent parser: `ArithParser` at `arithmetic.rs:37`, precedence climb via `parse_comparison` (89), `parse_expr` (139), `parse_term` (164), `parse_unary` (201), `parse_primary` (217).

### 4. VFS & Routing — **VALID**
- Longest-prefix matching: `router.rs:117-141` (`find_mount` compares `mount_path.as_os_str().len()`).
- Cross-mount `rename` rejected with `io::ErrorKind::Unsupported`: `router.rs:258-271` (Arc-pointer equality at 263).
- `resolve_real_path` bridges to backend's `real_path()` for external commands: `router.rs:101-104`.

### 5. Lexer Complexity & Heredoc Spans — **VALID**
- Multi-pass design: `preprocess_arithmetic` (`lexer.rs:1110`) and `preprocess_heredocs` (`lexer.rs:1272`), driver at `lexer.rs:1627-1632`.
- Marker substitution: arithmetic at `lexer.rs:1666-1671` (`__KAISH_ARITH_{id}__`); heredoc at `lexer.rs:1675-1688`.
- **Heredoc span tracking — explicitly skipped:** comment at `lexer.rs:1631` literally states "heredoc span tracking is not implemented for simplicity." Errors inside heredocs report incorrect line/column.

### 6. Scope COW Implementation — **VALID**
- `frames: Arc<Vec<HashMap<String, Value>>>` at `scope.rs:37`.
- `Arc::make_mut` clones the entire `Vec<HashMap>` on first mutation: `scope.rs:93, 125`.
- `${VAR.field}` only works for `$?`: `scope.rs:332-348`. Comment at 347-348: "For regular variables, only simple access is supported / No nested field access for regular variables." This is a real ergonomic gap given kaish's structured-data philosophy — every `--json` result you stash in a variable becomes opaque.

## Failure Mode Analysis

- **Uncaught Panics — VALID:** Sole panic site at `scope.rs:103` (`pop_frame` on root). Validator pairs `push_frame`/`pop_frame` for every block (`validator/walker.rs:206/215, 246/252, 267/271, 329/345`), so user scripts cannot reach it. Internal logic bugs still can.
- **Hidden Errors via stderr — VALID:** Same root cause as Finding 1; not exposed today because of fresh-kernel-per-execute, but real for shared-kernel embedders.
- **Silent Overflow — INVALID for arithmetic** (checked ops everywhere). Numeric literal parsing uses `i64::from_str` which returns a clean `Err`, so no silent overflow there either.

## Conclusion
The `kaish` kernel is a modern, structured take on the shell. The validation pass found no bogus claims — every finding is real in code.

**Status:**

1. **Concurrency contract — DONE.** Finding 1 is resolved via the per-kernel `execute_lock` plus `Kernel::fork`, with `BackendDispatcher` demoted to a test-only stub. Regression suite in `crates/kaish-kernel/tests/concurrency_tests.rs`.
2. **Heredoc span tracking** — still open. Small, isolated, silently degrades the diagnostic experience.
3. **`${VAR.field}` for regular JSON variables** — still open. Directly supports kaish's structured-data thesis.
