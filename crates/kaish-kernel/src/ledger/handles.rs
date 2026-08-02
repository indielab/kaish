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
///     let ApproverHandle(_inner, _authority, _principal) = handle;
/// }
/// ```
#[derive(Clone)]
pub struct ApproverHandle(Arc<LedgerInner>, AuthorityId, Principal);

/// Names the authority and the principal it decides as. Carries no ledger
/// state and no credential — there is nothing here a log line could leak.
impl std::fmt::Debug for ApproverHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApproverHandle")
            .field("authority", &self.1 .0)
            .field("principal", &self.2)
            .finish()
    }
}

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
            ApproverHandle(inner, AuthorityId(1), default_authority_principal(1)),
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
        self.0.settle(&attempt.request, attempt.attempt, outcome)
    }

    /// Abandon a request before any attempt was reserved against it (job
    /// discarded, session shutdown — spec §B.2). Fails with
    /// `LedgerError::AttemptInFlight` if an attempt is currently `Reserved`
    /// — settle or let the sweep close it first.
    pub async fn abandon_request(&self, id: &RequestId, reason: impl Into<String> + Send) -> Result<(), LedgerError> {
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
        self.grant_with_grounds(id, terms, Grounds::Embedder).await.map(|_| ())
    }

    /// Decide yes, recording *why* — the grounds the decision chain
    /// attaches when a `policy` or `decide` hook produced the decision
    /// (spec §A.4). [`Self::grant`] is this with `Grounds::Embedder`, which
    /// is what a direct embedder grant is.
    pub async fn grant_with_grounds(
        &self,
        id: &RequestId,
        terms: GrantTerms,
        grounds: Grounds,
    ) -> Result<Grant, LedgerError> {
        let decided_by = self.principal();
        self.0.grant(id, terms, decided_by, grounds)
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

    /// Stage 1 of the decision chain (spec §C.2): if a live standing grant
    /// covers this request, charge one use against it and post
    /// `Granted{grounds: Standing}` — match and consumption in the same
    /// critical section, so `max_uses` holds exactly under concurrency
    /// (spec §C.4, §B.1). `Ok(None)` means no rule covered the request and
    /// the caller falls through to stage 2.
    ///
    /// `grant_ttl` bounds the resulting grant, clamped down to the winning
    /// rule's own `expires_at`. The deciding principal on the grant is the
    /// rule's `issued_by` — whoever wrote the rule made this call, not
    /// whoever happens to hold this handle.
    ///
    /// Lives on the authority handle because it posts a `Granted` entry;
    /// [`DecisionChain`](super::DecisionChain) is the only caller.
    pub async fn grant_from_standing(
        &self,
        id: &RequestId,
        grant_ttl: Duration,
    ) -> Result<Option<Grant>, LedgerError> {
        self.0.grant_from_standing(id, grant_ttl)
    }

    /// How many uses have been charged against a standing grant so far
    /// (spec §C.4). The same number the log reconstructs by counting
    /// `Granted{grounds: Standing{id}}` entries; this is the cheap read.
    pub fn standing_uses(&self, id: &StandingId) -> u32 {
        self.0.standing_uses(*id)
    }

    /// Re-attribute this handle to `principal`. The returned handle shares
    /// the same ledger and the same minted authority — it grants exactly
    /// what this one grants — and records `principal` as the decider on
    /// every grant, denial, and credential retrieval made through it.
    ///
    /// This is the embedder's principal-assignment seam (spec §E.2, tier 2):
    /// the kernel enforces the requester/approver split and trusts the
    /// embedder to say who is who. `KernelConfig::with_principal` is what
    /// calls it in-tree.
    pub fn with_principal(mut self, principal: Principal) -> Self {
        self.2 = principal;
        self
    }

    /// The principal recorded as deciding/retrieving through this handle.
    /// Seeded to a distinct per-authority placeholder at mint time and
    /// replaced by [`Self::with_principal`] — which is what
    /// `KernelConfig::with_principal` does — so an embedder that never
    /// assigns identity still produces a name an auditor can distinguish
    /// from a real one.
    fn principal(&self) -> Principal {
        self.2.clone()
    }
}

/// The identity a freshly minted authority decides as until an embedder
/// assigns one. Deliberately not `Principal::default()` (an empty id), which
/// would collide with every other unnamed actor in the per-principal live
/// index.
fn default_authority_principal(authority: u64) -> Principal {
    Principal::new(
        format!("approver-handle-{authority}"),
        kaish_types::approval::PrincipalKind::Automation,
    )
}
