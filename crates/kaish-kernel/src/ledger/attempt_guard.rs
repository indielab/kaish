//! The dispatcher's drop-safe settlement guard (`docs/approval-ledger.md`
//! §C.1, ledger PR 3).
//!
//! Tools never call `settle` on the happy path. The obvious design — the
//! dispatch seam posts `Settled` after `tool.execute()` returns — does not
//! fire when the future is dropped, the task is aborted, the tool panics, or
//! the process dies: none of those paths run a line of code placed *after*
//! `tool.execute().await`. So settlement is a guard the dispatcher owns
//! instead: one `AttemptGuard` per attempt reserved during an invocation, its
//! normal-return path settling with the real outcome and its `Drop`
//! best-effort-settling `Outcome::Unknown` otherwise.
//!
//! Not wired into the generic dispatch path by this PR — no gate site calls
//! `ToolCtx::request_approval` for real yet (that begins at the ledger PR 5
//! cutover), so nothing in production constructs one of these today. The
//! type and its drop-safety are built and tested here so PR 5 has a
//! finished, proven primitive to wire in rather than a design to invent
//! under cutover pressure.

use kaish_types::approval::{LostCause, Outcome};

use super::error::LedgerError;
use super::handles::{AttemptHandle, Requester};

/// RAII wrapper around one reserved attempt. See the module doc for why this
/// exists instead of a plain post-execution `settle` call.
pub struct AttemptGuard {
    requester: Requester,
    attempt: AttemptHandle,
}

impl AttemptGuard {
    /// Wrap an attempt reserved via `Requester::redeem`/`redeem_with_token`.
    pub fn new(requester: Requester, attempt: AttemptHandle) -> Self {
        Self { requester, attempt }
    }

    /// The wrapped attempt's ids — for a caller that wants to report its own
    /// outcome via [`Self::settle`] or `ToolCtx::settle_with`.
    pub fn attempt(&self) -> &AttemptHandle {
        &self.attempt
    }

    /// Settle the attempt normally — the dispatcher's one call after
    /// `tool.execute()` returns with an exit code.
    ///
    /// Idempotent by `AttemptId` (spec §A.1): calling this before the guard
    /// drops means the `Drop` impl's own `Unknown` push, queued later,
    /// lands on an already-terminal attempt and appends nothing.
    pub async fn settle(&self, outcome: Outcome) -> Result<bool, LedgerError> {
        self.requester.settle(&self.attempt, outcome).await
    }
}

impl Drop for AttemptGuard {
    fn drop(&mut self) {
        // Synchronous and unconditional (spec §C.1): a destructor cannot
        // `.await` `Self::settle`, and checking "did this already settle?"
        // first would need the ledger's lock in a destructor too. Pushing
        // unconditionally is safe because settlement is idempotent by
        // `AttemptId` — a prior explicit `Self::settle` call already closed
        // the attempt, and this queued entry drains into a no-op.
        //
        // The outcome is always `Unknown`, never a bare "cancelled, nothing
        // happened": the tool may already have taken effect before its
        // future was dropped or it panicked, and the ledger's vocabulary
        // must not claim otherwise.
        self.requester.queue_drop_settle(
            &self.attempt,
            Outcome::Unknown {
                cause: LostCause::Cancelled,
            },
        );
    }
}
