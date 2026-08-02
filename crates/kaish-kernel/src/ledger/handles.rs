//! The public surface: three handles onto one [`LedgerInner`], split by
//! posting authority (spec §A.1). `Requester` posts obligations, `Approvals`
//! reads, `ApproverHandle` posts authorizations — and there is no method
//! anywhere in this file that lets a `Requester` produce a `Grant`.

use std::sync::Arc;
use std::time::Duration;

use kaish_types::approval::{
    ApprovalRequest, ApprovalRequestDraft, AttemptId, AttemptState, Capture, Grant, GrantTerms,
    Grounds, LedgerEntry, Observation, Outcome, Principal, RequestContext, RequestId,
    RequestState, StandingGrant, StandingId, Token,
};

use super::core::{build_inner, LedgerInner, SystemWallClock};
use super::config::{LedgerConfig, LedgerSink};
use super::error::LedgerError;

/// The implementation side's handle (spec §A.1). Obtained from `ExecContext`
/// / `ToolCtx` (PR 3). Can post obligations (`Requested`, `Redeemed`,
/// `Settled`) and read everything. Cannot grant — there is no method on this
/// type that produces a [`Grant`].
#[derive(Clone)]
pub struct Requester(Arc<LedgerInner>);

/// The read side (spec §A.1). Safe to hand to anyone: pending requests,
/// states, the log tail. Posts nothing.
#[derive(Clone)]
pub struct Approvals(Option<Arc<LedgerInner>>);

/// The approval side's capability (spec §A.1). No public constructor — the
/// only way to get one is [`Ledger::build`]. Not `Default`, not
/// `Deserialize`, not reachable from script or tool code.
///
/// # Examples
///
/// This type's fields are private, so building one outside this crate is a
/// compile error — that boundary IS the type-system enforcement tier of the
/// spec's enforcement ladder (§E.2, tier 1). The example below is written to
/// fail to compile as the proof; `cargo test --doc` runs it on every gate:
///
/// ```compile_fail
/// use kaish_kernel::ledger::ApproverHandle;
///
/// // The tuple fields are private outside `kaish_kernel::ledger` — a
/// // downstream crate (what a doctest compiles as) can neither construct
/// // one from parts nor pattern the fields back out of one it already
/// // holds, e.g. from a real `Ledger::build` call.
/// fn peel(handle: ApproverHandle) {
///     let ApproverHandle(_inner, _authority) = handle;
/// }
/// ```
#[derive(Clone)]
pub struct ApproverHandle(Arc<LedgerInner>, AuthorityId);

/// A lightweight tag distinguishing which minted authority a clone of
/// [`ApproverHandle`] descends from. PR 2 mints exactly one per
/// [`Ledger::build`] call and every clone shares it; nothing in this PR
/// branches on it yet (`deny_self_approval` and multi-authority embedders
/// are later-PR territory) — kept because the spec's own type signature
/// (`ApproverHandle(Arc<LedgerInner>, AuthorityId)`) carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AuthorityId(u64);

/// What one execution reserved against a grant. Exposes only its own two
/// ids — never provenance (spec §C.1): a tool cannot tell whether its grant
/// came from a human, a policy hook, or a standing rule.
#[derive(Debug, Clone)]
pub struct AttemptHandle {
    request: RequestId,
    attempt: AttemptId,
}

impl AttemptHandle {
    /// The request this attempt was reserved against.
    pub fn request_id(&self) -> &RequestId {
        &self.request
    }

    /// This attempt's id.
    pub fn attempt_id(&self) -> AttemptId {
        self.attempt
    }
}

/// The full read-model of one request: its view, current state, grant (if
/// any), and every attempt reserved against it (spec §D.2's `Approvals::get`).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct RequestChain {
    /// The request itself (tokenless — see `ApprovalRequestView`).
    pub request: kaish_types::approval::ApprovalRequestView,
    /// Current top-level state.
    pub state: RequestState,
    /// The grant, once decided.
    pub grant: Option<Grant>,
    /// Every attempt reserved against this request, in no particular order.
    pub attempts: Vec<AttemptView>,
}

/// One attempt's read-model: its id, state, and outcome once settled.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct AttemptView {
    /// The attempt's id.
    pub attempt: AttemptId,
    /// `Reserved`, `Settled`, or `Abandoned`.
    pub state: AttemptState,
    /// What it settled with, once `Settled`.
    pub outcome: Option<Outcome>,
}

/// Constructs a fresh ledger and its three handles. Not itself a public
/// type — `Ledger::build` is the only entry point (spec's `Ledger` is the
/// log; there is no separate `Ledger` value to hold onto afterward, only the
/// handles).
pub struct Ledger;

impl Ledger {
    /// Build a new, empty ledger and its three handles: obligations
    /// (`Requester`), read (`Approvals`), and the one minted authority
    /// (`ApproverHandle`). `sink` is optional — a ledger with none is purely
    /// in-memory and never applies sink backpressure (spec §D.4).
    ///
    /// # Errors
    ///
    /// Only if the OS cannot supply entropy for the ledger's id epoch (spec
    /// §A.2) — the same no-fallback stance `nonce.rs` takes on credentials.
    pub fn build(
        config: LedgerConfig,
        sink: Option<Arc<dyn LedgerSink>>,
    ) -> Result<(Requester, Approvals, ApproverHandle), getrandom::Error> {
        let inner = build_inner(config, sink, Arc::new(SystemWallClock))?;
        Ok((
            Requester(Arc::clone(&inner)),
            Approvals(Some(Arc::clone(&inner))),
            ApproverHandle(inner, AuthorityId(1)),
        ))
    }
}

impl Requester {
    /// Post a request. `draft` carries only what a producer supplies
    /// (`ApprovalRequest::builder`, spec §D.1); every other field is
    /// stamped here — the caller cannot forge a principal or an invocation.
    #[allow(clippy::too_many_arguments)]
    pub async fn post_request(
        &self,
        draft: ApprovalRequestDraft,
        principal: Principal,
        capture: Capture,
        context: RequestContext,
        ttl: Duration,
        job_id: Option<u64>,
    ) -> Result<ApprovalRequest, LedgerError> {
        self.0.post_request(draft, principal, capture, context, ttl, job_id)
    }

    /// Reserve an attempt by name, using an internal redemption context
    /// (the replay path — spec §B.4) rather than a presented credential.
    /// `observed` is what a `StateResolver` (PR 6) saw for each of the
    /// grant's conditions, evaluated *outside* the ledger lock and carried
    /// in (spec §B.1); pass an empty `Vec` for an unconditioned grant.
    pub async fn redeem(
        &self,
        id: &RequestId,
        by: Principal,
        observed: Vec<Observation>,
    ) -> Result<AttemptHandle, LedgerError> {
        // A stale queued `Unknown` for this chain's live attempt would leave
        // `live_attempt` looking occupied to this call — drain first so a
        // dropped guard's settlement lands before the reservation check
        // reads it (spec §C.1's "drains on its next append").
        self.0.drain_outbox();
        self.0
            .redeem(id, by, observed)
            .map(|attempt| AttemptHandle {
                request: id.clone(),
                attempt,
            })
    }

    /// Reserve an attempt by presenting a bearer credential (the
    /// `--confirm=<token>` path — spec §E.1). `id` is the request the
    /// presentation claims to be for (in the full system, resolved by the
    /// gate site's draft matcher, PR 5 — see this PR's flagged ambiguity in
    /// the PR body for why PR 2 takes it as a direct argument). A wrong
    /// `presented` value counts against `id`'s rejection total and, on the
    /// fifth, voids it (spec §F.3).
    pub async fn redeem_with_token(
        &self,
        id: &RequestId,
        presented: &str,
        by: Principal,
        observed: Vec<Observation>,
    ) -> Result<AttemptHandle, LedgerError> {
        // See `redeem`'s identical drain — the same `live_attempt` staleness
        // concern applies to the token-presentation path.
        self.0.drain_outbox();
        self.0
            .redeem_with_token(id, presented, by, observed)
            .map(|attempt| AttemptHandle {
                request: id.clone(),
                attempt,
            })
    }

    /// Report an attempt's terminal outcome. Idempotent by `AttemptId`
    /// (spec §A.1): settling an already-terminal attempt appends nothing
    /// and returns `Ok`. Returns whether a new entry was actually appended.
    pub async fn settle(&self, attempt: &AttemptHandle, outcome: Outcome) -> Result<bool, LedgerError> {
        self.0.drain_outbox();
        self.0.settle(&attempt.request, attempt.attempt, outcome)
    }

    /// Settle by explicit ids rather than this crate's opaque
    /// [`AttemptHandle`] — for `ExecContext::settle_with` (ledger PR 3),
    /// which receives a `kaish_tool_api::AttemptHandle` (a distinct type;
    /// see that crate's `approval` module for why) and has only
    /// `request_id()`/`attempt_id()` to work with, not this handle's
    /// private fields. Same idempotency and drain contract as
    /// [`Self::settle`].
    pub async fn settle_by_ids(
        &self,
        request: &RequestId,
        attempt: AttemptId,
        outcome: Outcome,
    ) -> Result<bool, LedgerError> {
        self.0.drain_outbox();
        self.0.settle(request, attempt, outcome)
    }

    /// Queue a best-effort settlement for the next drain, without settling
    /// synchronously. For `AttemptGuard::drop` (PR 3, `kaish-kernel`'s
    /// dispatcher), which cannot `.await` `Self::settle` from a destructor —
    /// see `LedgerInner::queue_outbox_settle`'s doc for the mechanism.
    ///
    /// Not part of the supported public surface — a tool has no legitimate
    /// reason to queue a settlement without also reserving the attempt, and
    /// this method exists purely for the dispatcher's own guard.
    #[doc(hidden)]
    pub fn queue_drop_settle(&self, attempt: &AttemptHandle, outcome: Outcome) {
        self.0.queue_outbox_settle(attempt.request.clone(), attempt.attempt, outcome);
    }

    /// Abandon a request before any attempt was reserved against it (job
    /// discarded, session shutdown — spec §B.2). Fails with
    /// `LedgerError::AttemptInFlight` if an attempt is currently `Reserved`
    /// — settle or let the sweep close it first.
    pub async fn abandon_request(&self, id: &RequestId, reason: impl Into<String> + Send) -> Result<(), LedgerError> {
        // An attempt whose guard already dropped (queued but not yet
        // drained) would wrongly still look `Reserved` to this call's
        // `AttemptInFlight` check.
        self.0.drain_outbox();
        self.0.abandon_request(id, reason.into())
    }

    /// Renew an `Expired` request: post a new `Requested` carrying the
    /// original's operation, resources, capture, principal, and trace
    /// context, linked via `supersedes` (spec §B.5). Renewal is a requester
    /// action — the principal that owns the request may renew it without
    /// holding any authority.
    pub async fn renew(&self, id: &RequestId) -> Result<RequestId, LedgerError> {
        self.0.renew(id)
    }
}

impl Approvals {
    /// A view with no live ledger behind it — the default for a context
    /// that has none (a unit-test harness, a minimal embedder). Every
    /// method returns empty/`None`; grants nothing, because it cannot: it
    /// holds no `LedgerInner` to post against even internally.
    pub fn empty() -> Self {
        Self(None)
    }

    /// Every currently-`Requested` (undecided) request.
    pub fn pending(&self) -> Vec<kaish_types::approval::ApprovalRequestView> {
        self.0
            .as_ref()
            .map(|inner| inner.pending().into_iter().map(Into::into).collect())
            .unwrap_or_default()
    }

    /// The current top-level state of one request, if it exists.
    pub fn state(&self, id: &RequestId) -> Option<RequestState> {
        self.0.as_ref().and_then(|inner| inner.state(id))
    }

    /// The full read-model — request, decision, and every attempt.
    pub fn get(&self, id: &RequestId) -> Option<RequestChain> {
        self.0.as_ref().and_then(|inner| inner.chain(id))
    }

    /// Every live standing grant.
    pub fn standing(&self) -> Vec<StandingGrant> {
        self.0.as_ref().map(|inner| inner.standing()).unwrap_or_default()
    }

    /// The retained log, seq-cursored: every retained entry with
    /// `seq > since`. Pass `0` for the full retained tail.
    pub fn log(&self, since: u64) -> Vec<LedgerEntry> {
        self.0.as_ref().map(|inner| inner.log(since)).unwrap_or_default()
    }
}

impl ApproverHandle {
    /// Decide yes. Fails with `LedgerError::AlreadyDecided` if the request
    /// already has a decision, or `LedgerError::Terminal` if it is no
    /// longer `Requested` for any other reason (expired, voided, ...).
    pub async fn grant(&self, id: &RequestId, terms: GrantTerms) -> Result<(), LedgerError> {
        let decided_by = self.principal();
        self.0.grant(id, terms, decided_by, Grounds::Embedder).map(|_| ())
    }

    /// Decide no.
    pub async fn deny(&self, id: &RequestId, reason: &str) -> Result<(), LedgerError> {
        let by = self.principal();
        self.0.deny(id, reason.to_string(), by)
    }

    /// Issue a standing grant. `g.id` is overwritten with a
    /// ledger-allocated id regardless of what the caller set — the
    /// returned `StandingId` is the authoritative one (there is no
    /// separate draft type in `kaish-types` for a not-yet-issued
    /// `StandingGrant`; see this PR's flagged ambiguity in the PR body).
    /// **Pure record only** — matching a live request against this rule is
    /// PR 4's decision-chain job, not this method's.
    pub async fn grant_standing(&self, g: StandingGrant) -> Result<StandingId, LedgerError> {
        self.0.grant_standing(g)
    }

    /// Revoke a standing grant. Takes effect immediately for requests not
    /// yet granted; already-issued grants are unaffected (spec §C.4).
    pub async fn revoke_standing(&self, id: &StandingId, reason: &str) -> Result<(), LedgerError> {
        let by = self.principal();
        self.0.revoke_standing(*id, by, reason.to_string())
    }

    /// Retrieve the credential for a live, granted request. Appends
    /// `KeyRetrieved` naming this handle's principal (spec §A.2) —
    /// retrieval is the one place authority matters on the key path (spec
    /// §D.3): everything else works by presenting whatever key you hold.
    pub fn token_for(&self, id: &RequestId) -> Option<Token> {
        let by = self.principal();
        self.0.token_for(id, by)
    }

    /// The principal recorded as deciding/retrieving through this handle.
    /// PR 2 has no embedder-supplied identity to draw from yet (that is
    /// `KernelConfig::with_principal`, PR 4) — every grant/denial/retrieval
    /// through a PR-2-era `ApproverHandle` is attributed to a fixed
    /// `Embedder`-kind principal until then.
    fn principal(&self) -> Principal {
        Principal::new(
            format!("approver-handle-{}", self.1 .0),
            kaish_types::approval::PrincipalKind::Automation,
        )
    }
}
