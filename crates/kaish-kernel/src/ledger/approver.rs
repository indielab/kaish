//! The four-stage decision chain (`docs/approval-ledger.md` §C.2) and the
//! [`Approver`] hook an embedder installs into it.
//!
//! Four stages, tried in order, and the **first non-`Defer` wins**:
//!
//! 1. **Standing grants** — a pure ledger lookup with no hook and no I/O,
//!    the only stage that runs under the ledger lock (§C.4).
//! 2. **[`Approver::policy`]** — synchronous, on the request path,
//!    contractually non-blocking. Allowlists and risk-class rules.
//! 3. **[`Approver::decide`]** — async, may take minutes. Runs under a
//!    patient hold so a human's think time does not trip the script
//!    watchdog, and `select!`s against the cancellation token.
//! 4. **Defer all the way through** ⇒ the request stays `Requested` and the
//!    gate site returns exit 2. **This is today's behavior, byte for byte,
//!    and it is what a kernel with no `Approver` configured does** — the
//!    trait's two decision methods both default to `Decision::Defer`, so an
//!    empty impl changes nothing.
//!
//! **`decide` is never called while the ledger lock is held** (§B.1). The
//! chain's structure is what enforces it: stage 1 is one self-contained
//! ledger transaction that returns before stage 2 starts, and stages 2 and 3
//! take no lock at all — they call back into [`Approvals`] freely, which is
//! exactly what the deadlock-shaped test in `ledger_approver_tests.rs`
//! proves.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kaish_tool_api::PatientGuard;
use kaish_types::approval::{
    ApprovalRequest, ApprovalRequestView, Decision, Grant, Grounds, Principal, PrincipalKind,
};
use tokio_util::sync::CancellationToken;

use super::error::LedgerError;
use super::handles::{ApproverHandle, Approvals, Requester};

/// How long [`Approver::decide`] may hold the patient budget before the
/// watchdog fires anyway — 300 seconds, the number §C.3 gives the terminal
/// prompt. An approver whose human sits elsewhere overrides
/// [`Approver::decide_budget`].
pub const DEFAULT_DECIDE_BUDGET: Duration = Duration::from_secs(300);

/// How long a grant the chain issues stays redeemable if nothing redeems it.
/// The chain grants in order to let an operation proceed immediately, so
/// this is a short leash, not a standing authorization.
const DEFAULT_GRANT_TTL: Duration = Duration::from_secs(300);

/// The embedder's decision hook (spec §C.2).
///
/// Both decision methods are defaulted to `Decision::Defer`, so an embedder
/// implements only the half it cares about and an empty impl changes no
/// behavior. An approver receives the tokenless [`ApprovalRequestView`] — it
/// decides, it does not redeem. There is no path from this trait to a
/// credential and none should be added: the view type has no field for one
/// (§A.2), and retrieval lives on [`ApproverHandle`], which the chain never
/// hands out.
///
/// ```compile_fail
/// use kaish_kernel::ledger::Approver;
/// use kaish_types::approval::{ApprovalRequestView, Decision};
///
/// struct Peeker;
///
/// impl Approver for Peeker {
///     fn policy(&self, req: &ApprovalRequestView, _l: &kaish_kernel::ledger::Approvals) -> Decision {
///         // There is no credential on the view — this does not compile,
///         // in this or any other spelling.
///         let _ = &req.token;
///         Decision::Defer
///     }
/// }
/// ```
#[async_trait]
pub trait Approver: Send + Sync {
    /// Stage 2: synchronous, on the request path, **contractually
    /// non-blocking**. Suitable for allowlists, risk-class rules, and
    /// "never `git.push.force`, full stop". `ledger` is the read side —
    /// pending requests, states, the log tail; it grants nothing.
    ///
    /// Blocking here blocks the gate site and every other execution behind
    /// it. Anything that can take time belongs in [`Self::decide`].
    fn policy(&self, req: &ApprovalRequestView, ledger: &Approvals) -> Decision {
        let _ = (req, ledger);
        Decision::Defer
    }

    /// Stage 3: async, may take minutes — a human at a terminal, a dialog in
    /// an embedder's UI, a clearance model. The chain runs it under a
    /// patient hold bounded by [`Self::decide_budget`] and races it against
    /// cancellation, so a slow decision does not trip the script timeout and
    /// a cancelled execution does not wait for one.
    async fn decide(&self, req: &ApprovalRequestView) -> Decision {
        let _ = req;
        Decision::Defer
    }

    /// Who decided. Recorded on every grant and denial this approver
    /// produces, so `approvals log` distinguishes machine clearance from
    /// human judgment (spec §E.6). Defaults to an `Automation` principal
    /// named `approver`.
    fn principal(&self) -> Principal {
        Principal::new("approver", PrincipalKind::Automation)
    }

    /// How long [`Self::decide`] may run under its patient hold. The
    /// watchdog fires if the hold outlives this, so it is the real bound on
    /// a human's think time. Defaults to [`DEFAULT_DECIDE_BUDGET`].
    fn decide_budget(&self) -> Duration {
        DEFAULT_DECIDE_BUDGET
    }
}

/// Which stage of the chain produced an outcome (spec §C.2's four stages).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainStage {
    /// Stage 1 — a standing grant covered the request.
    Standing,
    /// Stage 2 — [`Approver::policy`] decided.
    Policy,
    /// Stage 3 — [`Approver::decide`] decided.
    Decide,
}

/// What the chain concluded. Only [`ChainOutcome::Granted`] authorizes
/// anything; every other variant fails closed at the gate site.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum ChainOutcome {
    /// A stage granted, and the `Granted` entry is on the log.
    Granted {
        /// The posted grant.
        grant: Grant,
        /// Which stage decided.
        stage: ChainStage,
    },
    /// A stage denied, and the `Denied` entry is on the log.
    Denied {
        /// Why.
        reason: String,
        /// Which stage decided.
        stage: ChainStage,
    },
    /// Every stage deferred. The request stays `Requested`, nothing was
    /// posted beyond the `Requested` entry the caller already wrote, and
    /// fulfilment happens out of band — the gate site returns exit 2 with
    /// the pending view (spec §C.2 stage 4).
    Deferred,
    /// Cancellation fired before or during [`Approver::decide`]. Nothing was
    /// posted and nothing was granted; the request is left exactly as the
    /// caller posted it.
    Cancelled,
}

/// What the chain needs from the execution it is deciding for: somewhere to
/// take a patient hold, and the token that cancels it.
///
/// [`ChainContext::detached`] is the no-execution form — no watchdog to
/// suspend and a token nobody fires — for embedder-driven and test callers.
pub struct ChainContext<'a> {
    patient: Option<&'a dyn PatientSource>,
    cancel: CancellationToken,
}

impl<'a> ChainContext<'a> {
    /// Bind the chain to a live execution: `patient` suspends its script
    /// timeout for the length of `decide`, `cancel` interrupts it.
    pub fn new(patient: &'a dyn PatientSource, cancel: CancellationToken) -> Self {
        Self {
            patient: Some(patient),
            cancel,
        }
    }

    /// No watchdog to suspend and a token nobody fires.
    pub fn detached() -> Self {
        Self {
            patient: None,
            cancel: CancellationToken::new(),
        }
    }

    fn hold(&self, budget: Duration) -> PatientGuard {
        match self.patient {
            Some(patient) => patient.patient(budget),
            None => PatientGuard::inert(),
        }
    }
}

/// Anything that can suspend a script-level timeout for a patient operation
/// — in practice `ExecContext`, via `ToolCtx::patient`.
///
/// A one-method trait rather than taking `&dyn ToolCtx` directly: the chain
/// needs exactly this one capability, and a test driver should not have to
/// implement a whole tool context to exercise the patient path.
pub trait PatientSource: Send + Sync {
    /// Suspend the script timeout for `budget`; the returned guard restores
    /// it on drop. See `ToolCtx::patient` for the full contract.
    fn patient(&self, budget: Duration) -> PatientGuard;
}

impl PatientSource for crate::ExecContext {
    fn patient(&self, budget: Duration) -> PatientGuard {
        kaish_tool_api::ToolCtx::patient(self, budget)
    }
}

/// The four-stage chain, holding everything a decision needs: the authority
/// that posts the outcome, the read side a policy hook consults, and the
/// embedder's hook when one is installed.
///
/// A chain with no approver is stages 1 and 4 only, which is today's
/// behavior: no standing rule means Defer means exit 2.
#[derive(Clone)]
pub struct DecisionChain {
    authority: ApproverHandle,
    approvals: Approvals,
    /// Derived from `authority`, and used for exactly one thing: abandoning
    /// a request whose execution was cancelled out from under a decision
    /// (see [`DecisionChain::undo_if_cancelled`]). Grants nothing.
    requester: Requester,
    approver: Option<Arc<dyn Approver>>,
    grant_ttl: Duration,
}

impl std::fmt::Debug for DecisionChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionChain")
            .field("authority", &self.authority)
            .field("approver", &self.approver.is_some())
            .field("grant_ttl", &self.grant_ttl)
            .finish()
    }
}

impl DecisionChain {
    /// Build a chain over one ledger's authority and read side.
    /// `approver` is `None` for a kernel with no decision hook — stages 2
    /// and 3 are then skipped outright.
    pub fn new(authority: ApproverHandle, approvals: Approvals, approver: Option<Arc<dyn Approver>>) -> Self {
        let (requester, _, _) = authority.join();
        Self {
            authority,
            approvals,
            requester,
            approver,
            grant_ttl: DEFAULT_GRANT_TTL,
        }
    }

    /// Override how long a grant this chain issues stays redeemable.
    pub fn with_grant_ttl(mut self, ttl: Duration) -> Self {
        self.grant_ttl = ttl;
        self
    }

    /// Whether an [`Approver`] is installed. Stages 2 and 3 do nothing when
    /// this is `false`.
    pub fn has_approver(&self) -> bool {
        self.approver.is_some()
    }

    /// Run the chain against an already-posted request.
    ///
    /// The caller posts `Requested` (that is the requester's obligation) and
    /// hands the stamped request here; the chain posts at most one decision
    /// against it. Stages run in order and the first non-`Defer` short-
    /// circuits — a later stage never sees a request an earlier one decided.
    ///
    /// # Errors
    ///
    /// Any ledger transaction failure: the request was already decided, it
    /// is terminal, the ring or sink is full, or an approver returned terms
    /// that widen the request it was shown.
    pub async fn decide(
        &self,
        request: &ApprovalRequest,
        ctx: &ChainContext<'_>,
    ) -> Result<ChainOutcome, LedgerError> {
        // Fail closed on a cancellation that fired before we got here: an
        // execution that is already unwinding must not acquire authority on
        // its way out.
        if ctx.cancel.is_cancelled() {
            return Ok(ChainOutcome::Cancelled);
        }

        // ── Stage 1: standing grants ────────────────────────────────
        // One self-contained ledger transaction. It returns — releasing the
        // lock — before any hook below is called.
        if let Some(grant) = self
            .authority
            .grant_from_standing(&request.id, self.grant_ttl)
            .await?
        {
            return self
                .undo_if_cancelled(
                    request,
                    ChainOutcome::Granted {
                        grant,
                        stage: ChainStage::Standing,
                    },
                    ctx,
                )
                .await;
        }

        let Some(approver) = self.approver.as_ref() else {
            return Ok(ChainOutcome::Deferred);
        };
        let view = ApprovalRequestView::from(request);

        // ── Stage 2: policy ─────────────────────────────────────────
        // No lock is held here. `policy` is handed the read side and may
        // consult it freely.
        match approver.policy(&view, &self.approvals) {
            Decision::Defer => {}
            decision => {
                return self
                    .post(request, decision, approver.as_ref(), ChainStage::Policy, ctx)
                    .await
            }
        }

        // ── Stage 3: decide ─────────────────────────────────────────
        // Under a patient hold so a human's think time does not trip the
        // script watchdog, and raced against cancellation so a cancelled
        // execution does not wait one out. Still no lock held.
        let decision = {
            let _hold = ctx.hold(approver.decide_budget());
            tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => return Ok(ChainOutcome::Cancelled),
                decision = approver.decide(&view) => decision,
            }
        };
        // A decision that arrived and a cancellation that arrived with it:
        // the cancellation wins, because posting a grant for an execution
        // that is unwinding authorizes something nobody will perform.
        if ctx.cancel.is_cancelled() {
            return Ok(ChainOutcome::Cancelled);
        }
        match decision {
            Decision::Defer => {}
            decision => {
                return self
                    .post(request, decision, approver.as_ref(), ChainStage::Decide, ctx)
                    .await
            }
        }

        // ── Stage 4: defer ──────────────────────────────────────────
        Ok(ChainOutcome::Deferred)
    }

    /// Post an approver's non-`Defer` decision through the authority handle,
    /// attributed to the approver's own principal.
    async fn post(
        &self,
        request: &ApprovalRequest,
        decision: Decision,
        approver: &dyn Approver,
        stage: ChainStage,
        ctx: &ChainContext<'_>,
    ) -> Result<ChainOutcome, LedgerError> {
        let principal = approver.principal();
        match decision {
            Decision::Grant(terms) => {
                // Terms that drop or alter a condition the request declared
                // are refused by the ledger itself (`LedgerError::
                // ConditionsWidened`, spec §A.4) — the check lives at the
                // one place every grant passes through, not here, so an
                // approver cannot route around it.
                let grounds = match stage {
                    // The rule that matched is the approver itself — the
                    // trait gives a policy hook no way to name a finer rule,
                    // and inventing one would put a made-up name in the
                    // audit record.
                    ChainStage::Policy => Grounds::Policy {
                        rule: principal.id.clone(),
                    },
                    _ => Grounds::Embedder,
                };
                let grant = self
                    .authority
                    .clone()
                    .with_principal(principal)
                    .grant_with_grounds(&request.id, terms, grounds)
                    .await?;
                self.undo_if_cancelled(request, ChainOutcome::Granted { grant, stage }, ctx)
                    .await
            }
            Decision::Deny { reason } => {
                self.authority
                    .clone()
                    .with_principal(principal)
                    .deny(&request.id, &reason)
                    .await?;
                // A denial needs no undo: it authorizes nothing, and
                // erasing it would lose the record of a real decision.
                Ok(ChainOutcome::Denied { reason, stage })
            }
            Decision::Defer => Ok(ChainOutcome::Deferred),
            // `Decision` is `#[non_exhaustive]`: a variant added upstream
            // without a case here must not silently mean "yes".
            _ => Ok(ChainOutcome::Deferred),
        }
    }

    /// Close the window between "we decided to grant" and "the grant is on
    /// the log": if cancellation fired anywhere in it, abandon the request,
    /// which kills the grant and drops its credential.
    ///
    /// The window cannot be removed. A grant commits inside the ledger's
    /// critical section, and the ledger deliberately knows nothing about
    /// cancellation tokens — teaching it would run one feature's plumbing
    /// through another's boundary for a check that would still race the
    /// instant before the lock. So the guarantee is stated as an outcome
    /// rather than as timing: **a cancelled execution never leaves a live
    /// grant behind.** A grant that beat the cancellation is undone by
    /// `Abandoned`, which is the same transition a discarded job takes
    /// (spec §B.2), and the record shows both the decision and its undoing
    /// rather than hiding either.
    ///
    /// A failure to undo is **loud**: it returns the ledger's error rather
    /// than the grant, because reporting `Granted` for an execution that is
    /// unwinding is the one answer that could authorize something nobody
    /// will perform.
    async fn undo_if_cancelled(
        &self,
        request: &ApprovalRequest,
        outcome: ChainOutcome,
        ctx: &ChainContext<'_>,
    ) -> Result<ChainOutcome, LedgerError> {
        if !ctx.cancel.is_cancelled() {
            return Ok(outcome);
        }
        let ChainOutcome::Granted { .. } = &outcome else {
            return Ok(outcome);
        };
        self.requester
            .abandon_request(
                &request.id,
                "the execution was cancelled before its grant could be used",
            )
            .await?;
        Ok(ChainOutcome::Cancelled)
    }
}
