//! The approval-ledger vocabulary: pure data plus serde, no behavior.
//!
//! These are the types the approval ledger (`docs/approval-ledger.md` — the
//! "spec" cited throughout this module) is spoken in: requests, grants, the
//! append-only [`LedgerEntry`] log, and the small id/enum types they name.
//! There is deliberately no ledger here — no state machine, no I/O, no
//! matching. The ledger core (`Ledger`, the `Requester`/`Approvals`/
//! `ApproverHandle` split, the transition tables) belongs to `kaish-kernel`,
//! and nothing here depends on it or on `kaish-glob` — a [`ResourcePattern`]
//! carries pattern *data* only.
//!
//! Two structural guarantees hold everywhere in this module:
//!
//! - **[`Token`] is never a field of any other type here, and never
//!   implements `Serialize`/`Deserialize`.** The redemption credential lives
//!   only in the kernel's credential index, keyed by [`RequestId`] (spec
//!   §A.2). Adding a `token: Token` field to any serialized type fails to
//!   compile — `Token` has no serde impls to derive against. The exhaustive
//!   field destructures in this module's tests pin each wide record's field
//!   list, so a new field cannot land unreviewed.
//! - **[`ApprovalRequestDraft`] has no `principal`, `capture`, `id`, `context`,
//!   or `requested_at` field.** A plugin building a request through
//!   [`ApprovalRequest::builder`] cannot forge any of them — the draft type has
//!   nowhere to put them. [`ApprovalRequestDraft::stamp`] is the only path to a
//!   postable [`ApprovalRequest`], and only a caller holding the kernel context
//!   that knows those values can call it (spec §D.1).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ───────────────────────── Identity: RequestId ─────────────────────────

/// The request's public name. Format `"req_{epoch:8hex}_{seq}"`, e.g.
/// `"req_9c1a4f2e_42"` — underscores throughout and no other separator,
/// because a hyphen ends a terminal's double-click selection and this id
/// exists to be copied. There is no short form: an id is printed in full and
/// accepted in full, so it can never be ambiguous between sessions sharing a
/// ledger (spec §A.2).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RequestId(String);

/// Why a string failed to parse as a [`RequestId`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RequestIdParseError {
    /// Missing the `"req_"` prefix.
    #[error("request id {0:?} does not start with \"req_\"")]
    BadPrefix(String),
    /// Not exactly `req_<epoch>_<seq>` — includes any short form (e.g. the
    /// epoch alone, with no `_<seq>` suffix).
    #[error("request id {0:?} must have the shape req_<8hex>_<seq> — there is no short form")]
    BadShape(String),
    /// The epoch segment is not exactly 8 lowercase hex characters.
    #[error("request id {0:?} epoch must be exactly 8 lowercase hex characters")]
    BadEpoch(String),
    /// The sequence segment is not a plain decimal integer.
    #[error("request id {0:?} sequence must be a decimal integer")]
    BadSeq(String),
}

impl RequestId {
    /// Build a `RequestId` from a ledger epoch and a monotonic sequence
    /// number. `epoch` renders as 8 lowercase hex digits. Kernel-internal in
    /// practice — the ledger core is the only allocator (spec §A.2).
    pub fn new(epoch: u32, seq: u64) -> Self {
        Self(format!("req_{epoch:08x}_{seq}"))
    }

    /// Parse a `RequestId` from its full-form text. Rejects anything not
    /// full-form — there is no short form to accept (spec §A.2).
    pub fn parse(s: &str) -> Result<Self, RequestIdParseError> {
        let rest = s
            .strip_prefix("req_")
            .ok_or_else(|| RequestIdParseError::BadPrefix(s.to_string()))?;
        let (epoch_hex, seq_str) = rest
            .split_once('_')
            .ok_or_else(|| RequestIdParseError::BadShape(s.to_string()))?;
        let epoch_ok = epoch_hex.len() == 8
            && epoch_hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !epoch_ok {
            return Err(RequestIdParseError::BadEpoch(s.to_string()));
        }
        let seq_ok = !seq_str.is_empty() && seq_str.bytes().all(|b| b.is_ascii_digit());
        if !seq_ok {
            return Err(RequestIdParseError::BadSeq(s.to_string()));
        }
        // Re-render instead of storing the input: equality and lookup are
        // string-based, so "req_9c1a4f2e_042" must become the canonical
        // "req_9c1a4f2e_42" or a hand-typed leading zero fails `approvals
        // grant` with "not found" against a kernel-allocated id.
        let seq: u64 = seq_str
            .parse()
            .map_err(|_| RequestIdParseError::BadSeq(s.to_string()))?;
        Ok(Self(format!("req_{epoch_hex}_{seq}")))
    }

    /// The id's text form, e.g. `"req_9c1a4f2e_42"`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The allocation sequence this id carries — the `42` in
    /// `req_9c1a4f2e_42`.
    ///
    /// This is what orders requests chronologically. Sorting the id *text*
    /// does not: `req_9c1a4f2e_10` sorts before `req_9c1a4f2e_9`, so a tenth
    /// request would list ahead of the ninth on every surface that
    /// enumerates them.
    ///
    /// Returns 0 for a value holding a non-canonical id. Both constructors
    /// render the canonical form, so that cannot happen through the public
    /// API; 0 sorts such a value first rather than panicking a listing.
    pub fn seq(&self) -> u64 {
        self.0
            .rsplit_once('_')
            .and_then(|(_, seq)| seq.parse().ok())
            .unwrap_or(0)
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for RequestId {
    type Err = RequestIdParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for RequestId {
    type Error = RequestIdParseError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<RequestId> for String {
    fn from(id: RequestId) -> String {
        id.0
    }
}

// ───────────────────────── Identity: Token ─────────────────────────

/// The redemption credential. 128 bits from `getrandom`, 32 lowercase hex
/// (minted in `kaish-kernel`, kaish #259) — this type only carries the value
/// once minted, so it stays dependency-light.
///
/// Deliberately has **no** `Display`, `Serialize`, or `Deserialize` impl, and
/// its `Debug` impl prints only a redacted prefix. The credential lives ONLY
/// in the kernel's credential index, keyed by [`RequestId`]; it is never a
/// field of any [`LedgerEntry`] or any other public type in this module
/// (spec §A.2), and never serialized to a sink or the VFS. A `Debug` impl
/// that prints the raw value would be a bug precisely because `{:?}` is the
/// format callers reach for without thinking — that is the one place a
/// secret leaks by accident, so it is the one place this type refuses.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// Wrap a raw credential value already minted elsewhere.
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// First 4 characters — enough to correlate a `TokenRejected` entry with
    /// the grant it was aimed at ([`Grant::token_prefix`]), never enough to
    /// redeem (spec §A.4).
    pub fn token_prefix(&self) -> String {
        self.0.chars().take(4).collect()
    }

    /// The raw credential. Named loudly, not `as_str`/`AsRef`, so a call site
    /// that reveals the secret is a `grep`-able `.reveal(` rather than an
    /// invisible trait-method call.
    pub fn reveal(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Token(redacted, prefix={})", self.token_prefix())
    }
}

// ───────────────────────── Identity: the rest ─────────────────────────

/// One execution reserved against a grant. Unique within a ledger. Allocated
/// by the reservation that creates the attempt, never by a caller (spec
/// §A.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttemptId(u64);

impl AttemptId {
    /// Wrap a raw attempt id. Kernel-internal in practice — see the type doc.
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The underlying sequence number.
    pub fn get(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for AttemptId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The id of a [`StandingGrant`]. Kernel-allocated on `StandingIssued`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StandingId(u64);

impl StandingId {
    /// Wrap a raw standing-grant id. Kernel-internal in practice.
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The underlying sequence number.
    pub fn get(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for StandingId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The id of a [`Subscription`] (spec §C.5). Kernel-allocated on
/// registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SubscriptionId(u64);

impl SubscriptionId {
    /// Wrap a raw subscription id. Kernel-internal in practice.
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The underlying sequence number.
    pub fn get(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for SubscriptionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A dotted operation id (`"fs.remove"`, `"trash.empty"`, `"git.push"`).
/// In-tree operations come from a closed enum in `kaish-kernel`; plugins
/// register a namespace prefix at tool-registration time and build ids
/// through [`Self::namespaced`] (spec §A.6, §D.1).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationId(String);

/// Namespace prefixes reserved for in-tree kernel operations. A plugin
/// cannot register under these — see [`OperationId::namespaced`].
const RESERVED_OPERATION_PREFIXES: &[&str] = &["fs", "trash"];

/// Why an [`OperationId`] could not be built.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OperationIdError {
    /// The dotted id (or one of its parts) was empty.
    #[error("operation id must not be empty")]
    Empty,
    /// A plugin tried to register under a kernel-reserved namespace.
    #[error("operation namespace {0:?} is reserved for in-tree kernel operations")]
    ReservedPrefix(String),
}

impl OperationId {
    /// Build an `OperationId` from an already-dotted string (`"git.push"`).
    /// Rejects only an empty string — reserved-prefix enforcement is
    /// [`Self::namespaced`]'s job, run once at plugin registration, not
    /// re-checked on every request.
    pub fn new(dotted: impl Into<String>) -> Result<Self, OperationIdError> {
        let dotted = dotted.into();
        if dotted.is_empty() {
            return Err(OperationIdError::Empty);
        }
        Ok(Self(dotted))
    }

    /// Build a plugin-namespaced `OperationId`: `namespaced("git", "push")`
    /// → `"git.push"`. Rejects the reserved `fs`/`trash` prefixes, which
    /// belong to the kernel — a plugin cannot pose as an in-tree operation
    /// (spec §A.6).
    pub fn namespaced(prefix: &str, rest: &str) -> Result<Self, OperationIdError> {
        if prefix.is_empty() || rest.is_empty() {
            return Err(OperationIdError::Empty);
        }
        if RESERVED_OPERATION_PREFIXES.contains(&prefix) {
            return Err(OperationIdError::ReservedPrefix(prefix.to_string()));
        }
        Ok(Self(format!("{prefix}.{rest}")))
    }

    /// The id's dotted text form.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for OperationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A glob-shaped operation match, e.g. `"git.commit"` or `"fs.*"`
/// (`StandingGrant::operations`). Pattern *data* only — matching is
/// `kaish-glob`'s job (kaish-types must not depend on it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationPattern(String);

impl OperationPattern {
    /// Wrap a raw pattern string.
    pub fn new(pattern: impl Into<String>) -> Self {
        Self(pattern.into())
    }

    /// The pattern's text form.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for OperationPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ───────────────────────── Principal ─────────────────────────

/// Who is asking, or who decided. Appears on both the request (who asked)
/// and the grant (who decided) — spec §A.3.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Principal {
    /// Opaque identifier within `kind`'s namespace.
    pub id: String,
    /// What kind of actor this is.
    pub kind: PrincipalKind,
}

impl Principal {
    /// Build a principal.
    pub fn new(id: impl Into<String>, kind: PrincipalKind) -> Self {
        Self { id: id.into(), kind }
    }
}

/// What kind of actor a [`Principal`] is. Seeded by
/// `KernelConfig::with_principal`, defaulting to `Unknown` (spec §A.3).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    /// An AI agent.
    Agent,
    /// A human.
    Human,
    /// Unattended automation (a cron-style rule, CI).
    Automation,
    /// Not classified.
    #[default]
    Unknown,
}

// ───────────────────────── Risk, resources, transitions ─────────────────────────

/// How hard a request is to walk back. Read by an approver and matched by
/// policy — it carries no redemption semantics of its own (spec §F.3).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    /// Trivially undoable.
    Reversible,
    /// Undoable with effort (e.g. the trash, a git revert).
    Recoverable,
    /// Not undoable.
    Irreversible,
}

/// A resource identity that is more than a path: a namespaced kind plus id,
/// and the state-transition claim being authorized, when there is one
/// (spec §A.3).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resource {
    /// Namespace of the identifier. In-tree: `"path"`. Plugin-registered:
    /// `"git.ref"`, `"git.remote"`, `"git.worktree"`, `"url"`, `"job"`.
    pub kind: String,
    /// Identifier within that namespace (`"/home/a/x.txt"`,
    /// `"refs/heads/main"`, `"origin"`).
    pub id: String,
    /// The state-transition claim being authorized, when there is one.
    pub transition: Option<Transition>,
}

impl Resource {
    /// A resource with a declared before/after state claim.
    pub fn transition(
        kind: impl Into<String>,
        id: impl Into<String>,
        from: StateClaim,
        to: StateClaim,
    ) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
            transition: Some(Transition { from, to }),
        }
    }

    /// A resource with no transition claim (e.g. `git.remote: origin`).
    pub fn plain(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
            transition: None,
        }
    }

    /// Project to the bare `(kind, id)` pair, dropping the transition claim.
    pub fn to_ref(&self) -> ResourceRef {
        ResourceRef {
            kind: self.kind.clone(),
            id: self.id.clone(),
        }
    }

    /// The redemption-time [`Condition`] this resource implies: "the world
    /// must still show `transition.from`". `None` when the resource declared
    /// no transition — nothing to re-check at redemption.
    pub fn to_condition(&self) -> Option<Condition> {
        self.transition.as_ref().map(|t| Condition {
            resource: self.to_ref(),
            expected_from: t.from.clone(),
        })
    }
}

/// Names a resource without its transition claim — the pair an
/// [`Observation`] or a match result points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRef {
    /// Namespace of the identifier (see [`Resource::kind`]).
    pub kind: String,
    /// Identifier within that namespace (see [`Resource::id`]).
    pub id: String,
}

/// A before/after state claim on one [`Resource`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    /// The claimed state before the operation.
    pub from: StateClaim,
    /// The claimed state after the operation.
    pub to: StateClaim,
}

/// One side of a [`Transition`]: what a resource's state is claimed to be.
///
/// `Unspecified` is a distinct variant, not a wildcard: it never compares
/// equal to any concrete claim (`Absent`/`Exact`/`Digest`), including
/// another `Unspecified` compared against a concrete one. It only equals
/// itself. This is ordinary derived enum equality — the point is that no
/// custom `PartialEq` ever gives it wildcard semantics (spec §A.3).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateClaim {
    /// The resource does not exist (pre: creating; post: deleting).
    Absent,
    /// An opaque identifier the producer will re-derive at redemption: a git
    /// oid, an etag, a generation number.
    Exact(String),
    /// A content digest.
    Digest {
        /// The digest algorithm (e.g. `"sha256"`).
        alg: String,
        /// The digest, hex-encoded.
        hex: String,
    },
    /// "I don't claim anything about this side." Legal, but a grant whose
    /// conditions are all `Unspecified` records that fact so an auditor can
    /// see which approvals were unconditioned.
    Unspecified,
}

// ───────────────────────── Capture ─────────────────────────

/// The exact captured invocation of a gated tool call: the argv the approval
/// side would replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invocation {
    /// The dispatch name of the gated tool.
    pub tool: String,
    /// The captured argv.
    pub argv: Vec<String>,
}

/// Whether this invocation can be replayed by the approval side, and why not
/// when it cannot. Never a silently-empty argv (spec §B.4).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capture {
    /// Replayable by the approval side.
    Exact(Invocation),
    /// A direct `tool.execute` with no dispatch seam above it (a unit test).
    DirectExecution,
    /// The invocation cannot be represented as argv without loss.
    Unavailable {
        /// Why capture is not possible for this call shape.
        reason: String,
    },
    /// Capture was attempted and failed.
    CaptureFailed {
        /// What went wrong while capturing.
        reason: String,
    },
}

// ───────────────────────── Request context (tracing) ─────────────────────────

/// W3C trace context captured at request time, so an approval granted long
/// after the request still nests under the originating trace (spec §A.3,
/// §G).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RequestContext {
    /// W3C `traceparent`, if one was live at request time.
    pub traceparent: Option<String>,
    /// W3C `tracestate`, if one was live at request time.
    pub tracestate: Option<String>,
    /// The baggage subset captured at request time.
    pub baggage: BTreeMap<String, String>,
}

// ───────────────────────── ApprovalRequest + builder ─────────────────────────

/// The request entry: one privileged operation asking to proceed (spec
/// §A.3). Posted by the implementation side; every field except the ones a
/// producer supplies through [`ApprovalRequest::builder`] is stamped by the
/// kernel (`id`, `principal`, `capture`, `context`, `requested_at`, `ttl`,
/// `job_id`) — see [`ApprovalRequestDraft::stamp`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// The request's public name.
    pub id: RequestId,
    /// Dotted taxonomy (`"fs.remove"`, `"trash.empty"`, `"git.push"`).
    pub operation: OperationId,
    /// How hard this operation is to walk back.
    pub risk: RiskClass,
    /// The resources this operation would touch.
    pub resources: Vec<Resource>,
    /// Who is asking.
    pub principal: Principal,
    /// Whether this invocation can be replayed by the approval side.
    pub capture: Capture,
    /// W3C context captured at request time.
    pub context: RequestContext,
    /// The backgrounded job that raised this request, if any.
    pub job_id: Option<u64>,
    /// Why the gate fired.
    pub reason: String,
    /// Display-only re-run template. Producer-authored, therefore untrusted
    /// text (spec §C.3) — never contains a credential.
    pub hint: String,
    /// Wall-clock post time.
    #[serde(with = "crate::rfc3339::system_time")]
    pub requested_at: SystemTime,
    /// How long the request stays live with no decision.
    pub ttl: Duration,
    /// Set when this request renews an expired predecessor (spec §B.5).
    pub supersedes: Option<RequestId>,
}

impl ApprovalRequest {
    /// Start building a draft request for `operation` (a dotted id, e.g.
    /// `"git.push"`). The draft carries no principal, capture, id, context,
    /// or timing — those are kernel-stamped (spec §D.1).
    pub fn builder(operation: impl Into<String>) -> ApprovalRequestBuilder {
        REQUESTS_CONSTRUCTED.fetch_add(1, Ordering::Relaxed);
        ApprovalRequestBuilder {
            operation: operation.into(),
            risk: None,
            resources: Vec::new(),
            reason: String::new(),
            hint: String::new(),
            supersedes: None,
        }
    }

    /// How many approval requests this process has begun building, ever —
    /// one per [`ApprovalRequest::builder`] call, counted whether or not the
    /// draft is ever built, stamped, or posted.
    ///
    /// This exists to be asserted on. Spec §C.5 requires that an `fs.*`
    /// operation nothing is subscribed to allocate **no** request at all, no
    /// matter how many paths it touches, and a counter is the only way to
    /// state that in numbers: run a 10,000-path `rm -rf` between two reads
    /// and the difference must be 0. Relaxed ordering, because it is a
    /// process-wide diagnostic total and never a synchronization point —
    /// read it from one task at a time or accept that a concurrent builder
    /// may or may not be included.
    pub fn constructed_count() -> u64 {
        REQUESTS_CONSTRUCTED.load(Ordering::Relaxed)
    }
}

/// Backing store for [`ApprovalRequest::constructed_count`].
static REQUESTS_CONSTRUCTED: AtomicU64 = AtomicU64::new(0);

/// The public view of a request: everything in [`ApprovalRequest`] and
/// nothing else — deliberately no credential field, so there is nothing to
/// redact and nothing to leak through clone/serde/VFS/telemetry (spec §A.2).
/// This is what `ExecResult.approval`, `JobInfo.approval`, `/v/approvals`,
/// and an `Approver`'s input all see.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequestView {
    /// See [`ApprovalRequest::id`].
    pub id: RequestId,
    /// See [`ApprovalRequest::operation`].
    pub operation: OperationId,
    /// See [`ApprovalRequest::risk`].
    pub risk: RiskClass,
    /// See [`ApprovalRequest::resources`].
    pub resources: Vec<Resource>,
    /// See [`ApprovalRequest::principal`].
    pub principal: Principal,
    /// See [`ApprovalRequest::capture`].
    pub capture: Capture,
    /// See [`ApprovalRequest::context`].
    pub context: RequestContext,
    /// See [`ApprovalRequest::job_id`].
    pub job_id: Option<u64>,
    /// See [`ApprovalRequest::reason`].
    pub reason: String,
    /// See [`ApprovalRequest::hint`].
    pub hint: String,
    /// See [`ApprovalRequest::requested_at`].
    #[serde(with = "crate::rfc3339::system_time")]
    pub requested_at: SystemTime,
    /// See [`ApprovalRequest::ttl`].
    pub ttl: Duration,
    /// See [`ApprovalRequest::supersedes`].
    pub supersedes: Option<RequestId>,
}

impl From<ApprovalRequest> for ApprovalRequestView {
    fn from(req: ApprovalRequest) -> Self {
        Self {
            id: req.id,
            operation: req.operation,
            risk: req.risk,
            resources: req.resources,
            principal: req.principal,
            capture: req.capture,
            context: req.context,
            job_id: req.job_id,
            reason: req.reason,
            hint: req.hint,
            requested_at: req.requested_at,
            ttl: req.ttl,
            supersedes: req.supersedes,
        }
    }
}

impl From<&ApprovalRequest> for ApprovalRequestView {
    fn from(req: &ApprovalRequest) -> Self {
        req.clone().into()
    }
}

/// A producer-built draft: everything a caller supplies through
/// [`ApprovalRequest::builder`], and nothing the kernel must stamp. There is
/// no `principal` field, no `capture` field, no `id` field, no `context`
/// field, and no `requested_at` field — a plugin cannot forge a principal or
/// an invocation because the type has nowhere to put one (spec §D.1).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequestDraft {
    /// Dotted taxonomy.
    pub operation: OperationId,
    /// How hard this operation is to walk back.
    pub risk: RiskClass,
    /// The resources this operation would touch.
    pub resources: Vec<Resource>,
    /// Why the gate fired.
    pub reason: String,
    /// Display-only re-run template.
    pub hint: String,
    /// Set when this request renews an expired predecessor.
    pub supersedes: Option<RequestId>,
}

impl ApprovalRequestDraft {
    /// Stamp the kernel-supplied fields, turning a draft into a postable
    /// [`ApprovalRequest`]. Pure field assembly — no I/O, no validation
    /// beyond what the draft already carries. This method is `pub` and does
    /// not itself gate who calls it: the guarantee is that a *draft* cannot
    /// carry these fields, and the kernel's `request_approval` seam (ledger
    /// PR 3) is the one place real values enter.
    // One argument per kernel-stamped field (spec §D.1's exact list) reads
    // clearer here than a stamping-context struct for a 7-argument leaf
    // constructor with no optional subsets.
    #[allow(clippy::too_many_arguments)]
    pub fn stamp(
        self,
        id: RequestId,
        principal: Principal,
        capture: Capture,
        context: RequestContext,
        requested_at: SystemTime,
        ttl: Duration,
        job_id: Option<u64>,
    ) -> ApprovalRequest {
        ApprovalRequest {
            id,
            operation: self.operation,
            risk: self.risk,
            resources: self.resources,
            principal,
            capture,
            context,
            job_id,
            reason: self.reason,
            hint: self.hint,
            requested_at,
            ttl,
            supersedes: self.supersedes,
        }
    }
}

/// Builder for [`ApprovalRequestDraft`]. See [`ApprovalRequest::builder`].
#[derive(Debug, Clone)]
pub struct ApprovalRequestBuilder {
    operation: String,
    risk: Option<RiskClass>,
    resources: Vec<Resource>,
    reason: String,
    hint: String,
    supersedes: Option<RequestId>,
}

/// Why an [`ApprovalRequestBuilder::build`] call failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ApprovalRequestBuildError {
    /// `operation` was empty — an unnamed operation cannot be judged, and
    /// the ledger's taxonomy check depends on a real dotted id (spec §A.6).
    #[error("approval request operation must not be empty")]
    EmptyOperation,
    /// `risk` was never set. There is no safe default: silently picking
    /// `Reversible` could downgrade an irreversible operation past a policy
    /// that keys on risk class.
    #[error("approval request risk class must be set explicitly — there is no safe default")]
    MissingRisk,
}

impl ApprovalRequestBuilder {
    /// Set the risk class. Required — [`Self::build`] fails without it.
    pub fn risk(mut self, risk: RiskClass) -> Self {
        self.risk = Some(risk);
        self
    }

    /// Add one resource this operation would touch.
    pub fn resource(mut self, resource: Resource) -> Self {
        self.resources.push(resource);
        self
    }

    /// Set why the gate fired.
    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = reason.into();
        self
    }

    /// Set the display-only re-run hint.
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    /// Mark this draft as renewing an expired predecessor.
    pub fn supersedes(mut self, id: RequestId) -> Self {
        self.supersedes = Some(id);
        self
    }

    /// Finish the draft. Fails when `operation` is empty or `risk` was never
    /// set.
    pub fn build(self) -> Result<ApprovalRequestDraft, ApprovalRequestBuildError> {
        let operation =
            OperationId::new(self.operation).map_err(|_| ApprovalRequestBuildError::EmptyOperation)?;
        let risk = self.risk.ok_or(ApprovalRequestBuildError::MissingRisk)?;
        Ok(ApprovalRequestDraft {
            operation,
            risk,
            resources: self.resources,
            reason: self.reason,
            hint: self.hint,
            supersedes: self.supersedes,
        })
    }
}

// ───────────────────────── Grant side ─────────────────────────

/// The authorization entry: one decision to allow a request (spec §A.4).
/// Posted by the approval side. There is no redemption-limit field — see the
/// comment on [`GrantTerms`] for why.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    /// The request this grant authorizes.
    pub request: RequestId,
    /// Who decided.
    pub decided_by: Principal,
    /// Why the decision was made, and by which mechanism.
    pub grounds: Grounds,
    /// The grant expires at this time if unredeemed.
    #[serde(with = "crate::rfc3339::system_time")]
    pub not_after: SystemTime,
    /// First 4 hex characters of the credential, for correlating a
    /// `TokenRejected` with the grant it was aimed at. The credential itself
    /// is never in an entry (spec §A.2).
    pub token_prefix: String,
    // No redemption-limit field. Every grant authorizes exactly one
    // successful settlement; failed attempts do not consume it (spec §A.1).
    // A rule that should fire repeatedly is a StandingGrant with `max_uses`
    // (spec §C.4).
    /// Preconditions re-verified at redemption. Defaults to exactly the
    /// transitions declared on the request's resources. An approver may
    /// narrow (add or tighten) and may never widen — enforced at post time.
    pub conditions: Vec<Condition>,
    /// Wall-clock decision time.
    #[serde(with = "crate::rfc3339::system_time")]
    pub decided_at: SystemTime,
}

impl Grant {
    /// Build a `Grant` from its terms plus the decision provenance. The only
    /// constructor this `#[non_exhaustive]` type has outside this crate —
    /// `token_prefix` is computed by the caller from the freshly minted
    /// [`Token`] (never stored here — spec §A.2), so it is threaded through
    /// rather than derived from anything already on `terms`.
    pub fn from_terms(
        request: RequestId,
        decided_by: Principal,
        grounds: Grounds,
        terms: GrantTerms,
        token_prefix: String,
        decided_at: SystemTime,
    ) -> Self {
        Self {
            request,
            decided_by,
            grounds,
            not_after: terms.not_after,
            token_prefix,
            conditions: terms.conditions,
            decided_at,
        }
    }
}

/// The terms an [`Decision::Grant`] carries before the kernel turns them
/// into a full [`Grant`] (which also records `request`, `decided_by`, and
/// `decided_at`).
///
/// No redemption-count field here either, for the same reason as `Grant`:
/// single-successful-redemption is the rule, not a configurable limit.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrantTerms {
    /// The grant expires at this time if unredeemed.
    #[serde(with = "crate::rfc3339::system_time")]
    pub not_after: SystemTime,
    /// Preconditions re-verified at redemption.
    pub conditions: Vec<Condition>,
}

impl GrantTerms {
    /// A one-shot grant good until `not_after`, with conditions defaulted to
    /// exactly the transitions the request declared on its resources (spec
    /// §A.4).
    pub fn once_for(req: &ApprovalRequest, not_after: SystemTime) -> Self {
        let conditions = req.resources.iter().filter_map(Resource::to_condition).collect();
        Self { not_after, conditions }
    }

    /// The same terms, from the tokenless [`ApprovalRequestView`] an
    /// approver actually holds.
    ///
    /// **This is what an approver should call.** An approver never sees the
    /// stamped [`ApprovalRequest`], and rebuilding one from a view to reach
    /// [`Self::once_for`] drops the request's resources unless the caller
    /// remembers to copy them one by one — which produces terms with no
    /// conditions, and the ledger rejects those as widening (spec §A.4).
    pub fn once_for_view(view: &ApprovalRequestView, not_after: SystemTime) -> Self {
        let conditions = view.resources.iter().filter_map(Resource::to_condition).collect();
        Self { not_after, conditions }
    }

    /// Build terms directly from an explicit condition list. The only other
    /// external constructor for this `#[non_exhaustive]` type besides
    /// [`Self::once_for`] and [`Self::once_for_view`] — an approver that
    /// narrows (adds or tightens) beyond the request's declared transitions,
    /// rather than accepting them verbatim, needs this rather than a struct
    /// literal.
    pub fn new(not_after: SystemTime, conditions: Vec<Condition>) -> Self {
        Self { not_after, conditions }
    }
}

/// Why a request was granted, and by which mechanism (spec §A.4).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Grounds {
    /// A human said yes.
    Human {
        /// Distinguishes the REPL terminal from an embedder's out-of-band UI.
        channel: String,
    },
    /// The embedder's synchronous policy hook.
    Policy {
        /// The rule that matched.
        rule: String,
    },
    /// A standing grant already in the ledger fired.
    Standing {
        /// The standing grant that produced this decision.
        grant: StandingId,
    },
    /// An `observe` subscription matched (spec §C.5). Records the operation
    /// and proceeds; carries no permission semantics.
    Observe {
        /// The subscription that matched.
        subscription: SubscriptionId,
    },
    /// The embedder granted directly through its `ApproverHandle`.
    Embedder,
}

/// A precondition re-verified at redemption: "the world must still show
/// `expected_from` for `resource`" (spec §B.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Condition {
    /// The resource this condition constrains.
    pub resource: ResourceRef,
    /// The state the resource must still be in.
    pub expected_from: StateClaim,
}

/// What a redemption-time condition check saw, and when (spec §A.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// The resource observed.
    pub resource: ResourceRef,
    /// What was observed.
    pub claim: StateClaim,
    /// When it was observed.
    #[serde(with = "crate::rfc3339::system_time")]
    pub at: SystemTime,
}

/// A rule that auto-grants matching future requests. Itself a ledger entry
/// (`StandingIssued`) — every request it auto-approves produces a normal
/// `Granted` entry naming it (spec §C.4).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StandingGrant {
    /// This standing grant's id.
    pub id: StandingId,
    /// Operation patterns this rule covers (e.g. `"git.commit"`, `"fs.*"`).
    pub operations: Vec<OperationPattern>,
    /// Resource patterns this rule covers.
    pub resources: Vec<ResourcePattern>,
    /// Restrict to one requesting principal; `None` means any requester in
    /// this session.
    pub principal: Option<Principal>,
    /// Maximum number of matching requests this rule may auto-approve.
    /// Defaults to 1 — a standing rule is one-shot unless explicitly
    /// widened ([`Self::with_max_uses`] / [`Self::unlimited_uses`]).
    /// `None` is explicit unlimited; an omitted field on the wire is the
    /// one-shot default, never unlimited. **On the wire, `"max_uses":
    /// null` reads as explicit unlimited** — a producer that means "use
    /// the default" must omit the field, not send null.
    #[serde(default = "default_max_uses")]
    pub max_uses: Option<u32>,
    /// When this rule stops matching, if it expires.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::rfc3339::opt_system_time"
    )]
    pub expires_at: Option<SystemTime>,
    /// Who issued this rule.
    pub issued_by: Principal,
    /// Why this rule exists.
    pub reason: String,
}

/// The wire and constructor default for [`StandingGrant::max_uses`]:
/// one-shot. Wider is always an explicit act.
fn default_max_uses() -> Option<u32> {
    Some(1)
}

impl StandingGrant {
    /// Build a not-yet-issued standing grant, one-shot by default
    /// (`max_uses = Some(1)`) — widen explicitly with
    /// [`Self::with_max_uses`] or [`Self::unlimited_uses`]. `id` is a
    /// placeholder — `ApproverHandle::grant_standing` overwrites it with a
    /// ledger-allocated [`StandingId`] when the rule is issued (spec §C.4);
    /// there is no separate draft type here for the same reason
    /// [`ApprovalRequestDraft`] exists for [`ApprovalRequest`]. The only
    /// external constructor for this `#[non_exhaustive]` type.
    pub fn new(
        operations: Vec<OperationPattern>,
        resources: Vec<ResourcePattern>,
        principal: Option<Principal>,
        expires_at: Option<SystemTime>,
        issued_by: Principal,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            id: StandingId::new(0),
            operations,
            resources,
            principal,
            max_uses: default_max_uses(),
            expires_at,
            issued_by,
            reason: reason.into(),
        }
    }

    /// Widen the rule to auto-approve up to `n` matching requests.
    pub fn with_max_uses(mut self, n: u32) -> Self {
        self.max_uses = Some(n);
        self
    }

    /// Remove the use bound entirely. Unlimited is an explicit act, never
    /// a default — say so in `reason`.
    pub fn unlimited_uses(mut self) -> Self {
        self.max_uses = None;
        self
    }
}

/// A resource-matching pattern (`{ kind: "git.ref", pattern:
/// "refs/heads/agent/*" }`). Pattern *data* only — matching is
/// `kaish-glob`'s job (kaish-types must not depend on it); kind must match
/// exactly, only `id` globs (spec §C.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourcePattern {
    /// Namespace of the identifier (see [`Resource::kind`]) — matched
    /// exactly, never globbed.
    pub kind: String,
    /// A glob pattern over [`Resource::id`].
    pub pattern: String,
}

impl ResourcePattern {
    /// Build a resource pattern.
    pub fn new(kind: impl Into<String>, pattern: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            pattern: pattern.into(),
        }
    }
}

/// A glob-scoped registration making matching operations `observe` (record
/// only) or `enforce` (decide) — spec §C.5.
///
/// Itself a ledger entry (`Subscribed`), and so is its revocation
/// (`Unsubscribed`): an audit record whose own scope changed without a
/// record of the change would be unreadable.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    /// This subscription's id.
    pub id: SubscriptionId,
    /// Operation patterns this subscription covers.
    pub operations: Vec<OperationPattern>,
    /// Resource patterns this subscription covers.
    pub resources: Vec<ResourcePattern>,
    /// Whether matching operations are recorded only, or go through the
    /// real decision chain.
    pub mode: SubscriptionMode,
    /// Why this subscription exists.
    pub reason: String,
}

impl Subscription {
    /// Build a not-yet-registered subscription. `id` is a placeholder —
    /// `ApproverHandle::subscribe` overwrites it with a ledger-allocated
    /// [`SubscriptionId`], and the returned id is the authoritative one
    /// (same shape as [`StandingGrant::new`], for the same reason). The only
    /// external constructor for this `#[non_exhaustive]` type.
    pub fn new(
        operations: Vec<OperationPattern>,
        resources: Vec<ResourcePattern>,
        mode: SubscriptionMode,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            id: SubscriptionId::new(0),
            operations,
            resources,
            mode,
            reason: reason.into(),
        }
    }
}

/// The two subscription modes (spec §C.5) — the audit-versus-enforce split.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionMode {
    /// Matching operations post `Requested` + immediate `Granted{Observe}`
    /// and proceed; they never defer, never block, never prompt.
    Observe,
    /// Matching operations go through the real decision chain (spec §C.2).
    Enforce,
}

/// An `Approver`'s verdict on a request (spec §C.2).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Grant, on these terms.
    Grant(GrantTerms),
    /// Deny, with a reason.
    Deny {
        /// Why the request was denied.
        reason: String,
    },
    /// "Not my call." Falls through to the next decision-chain stage. Never
    /// means "yes".
    Defer,
}

// ───────────────────────── Attempt outcome ─────────────────────────

/// How a redeemed attempt ended (spec §A.5).
///
/// Externally tagged (`{"exit":0}`, `{"unknown":{"cause":"cancelled"}}`) —
/// unlike [`Grounds`] or [`LedgerEntry`], `Exit`/`Error` wrap a bare scalar
/// rather than a struct, and serde cannot represent a scalar-wrapping
/// newtype variant under internal tagging.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The tool ran and reported a POSIX exit code.
    Exit(i64),
    /// The tool ran and reported an error.
    Error(String),
    /// The attempt's executor went away before reporting an exit code. The
    /// operation may already have taken effect — this outcome never means
    /// "nothing happened", which is why there is no `Cancelled` variant.
    Unknown {
        /// Why the executor is presumed lost.
        cause: LostCause,
    },
}

/// Why an attempt's executor is presumed lost (spec §A.5).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LostCause {
    /// The dispatcher's `AttemptGuard` was dropped (cooperative
    /// cancellation, a panic, an aborted task).
    Cancelled,
    /// The recovery sweep found a reservation nobody reported on.
    ExecutorLost,
}

/// The three states of one attempt (spec §B.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    /// Reservation committed; no terminal report yet.
    Reserved,
    /// Something reported: an exit code, an error, or `Outcome::Unknown`.
    Settled,
    /// The recovery sweep found a reservation nobody reported on.
    Abandoned,
}

/// The top-level state of one request (spec §B.2's request state machine).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    /// Posted; awaiting a decision.
    Requested,
    /// Decided yes. May still be reserving/settling attempts.
    Granted,
    /// Decided no.
    Denied,
    /// TTL (or grant `not_after`) elapsed with no closing decision.
    Expired,
    /// Discarded (job discarded, session shutdown) before authorizing an
    /// execution.
    Abandoned,
    /// Preconditions failed, or 5 rejected credentials — dead, re-request
    /// required.
    Voided,
}

/// What a `Expired` entry's `what` names: which TTL elapsed (spec §B.1).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expiring {
    /// The request's own TTL — nobody decided in time.
    Request,
    /// The grant's `not_after` — decided, but never (successfully) redeemed
    /// in time.
    Grant,
}

// ───────────────────────── The entry log ─────────────────────────

/// One append to the ledger. Internally tagged on the `"entry"` key so
/// NDJSON stays one self-describing line per entry (spec §A.5). `seq` is
/// monotonic per ledger; `at` is wall-clock and exists purely for the
/// record — expiry math never uses it (see the module-level warning on
/// `kaish_types::clock::Instant` vs. wall-clock jumps, spec §A.5).
///
/// No entry carries a credential, so the whole log is safe to stream to a
/// sink, project into `/v/approvals`, and print.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
pub enum LedgerEntry {
    /// The implementation side posted a request.
    Requested {
        /// Monotonic per-ledger sequence number.
        seq: u64,
        /// Wall-clock post time.
        #[serde(with = "crate::rfc3339::system_time")]
        at: SystemTime,
        /// The posted request.
        request: ApprovalRequest,
    },
    /// The approval side posted a grant.
    Granted {
        /// Monotonic per-ledger sequence number.
        seq: u64,
        /// Wall-clock post time.
        #[serde(with = "crate::rfc3339::system_time")]
        at: SystemTime,
        /// The posted grant.
        grant: Grant,
    },
    /// The approval side posted a denial.
    Denied {
        /// Monotonic per-ledger sequence number.
        seq: u64,
        /// Wall-clock post time.
        #[serde(with = "crate::rfc3339::system_time")]
        at: SystemTime,
        /// The denied request.
        request: RequestId,
        /// Who denied it.
        by: Principal,
        /// Why.
        reason: String,
    },
    /// A TTL elapsed with no closing decision.
    Expired {
        /// Monotonic per-ledger sequence number.
        seq: u64,
        /// Wall-clock post time.
        #[serde(with = "crate::rfc3339::system_time")]
        at: SystemTime,
        /// The expired request.
        request: RequestId,
        /// Which TTL elapsed.
        what: Expiring,
    },
    /// The approval side retrieved the key. Appended on every retrieval, so
    /// a key that leaves the kernel has a name attached to its departure
    /// (spec §A.2).
    KeyRetrieved {
        /// Monotonic per-ledger sequence number.
        seq: u64,
        /// Wall-clock post time.
        #[serde(with = "crate::rfc3339::system_time")]
        at: SystemTime,
        /// The request whose key was retrieved.
        request: RequestId,
        /// Who retrieved it.
        by: Principal,
    },
    /// An attempt was reserved.
    Redeemed {
        /// Monotonic per-ledger sequence number.
        seq: u64,
        /// Wall-clock post time.
        #[serde(with = "crate::rfc3339::system_time")]
        at: SystemTime,
        /// The request being redeemed.
        request: RequestId,
        /// The attempt this reservation allocated.
        attempt: AttemptId,
        /// The principal that presented the key or held the redemption
        /// context — the other half of the accountability pair with
        /// `KeyRetrieved` (spec §A.2).
        by: Principal,
        /// What the condition check saw, and when.
        observed: Vec<Observation>,
    },
    /// Preconditions no longer hold. Voids the grant and reserves no
    /// attempt.
    Refused {
        /// Monotonic per-ledger sequence number.
        seq: u64,
        /// Wall-clock post time.
        #[serde(with = "crate::rfc3339::system_time")]
        at: SystemTime,
        /// The request whose redemption was refused.
        request: RequestId,
        /// The condition that failed.
        condition: Condition,
        /// What was actually observed.
        found: StateClaim,
    },
    /// An attempt reported a terminal outcome.
    Settled {
        /// Monotonic per-ledger sequence number.
        seq: u64,
        /// Wall-clock post time.
        #[serde(with = "crate::rfc3339::system_time")]
        at: SystemTime,
        /// The request this attempt belongs to.
        request: RequestId,
        /// The attempt that settled.
        attempt: AttemptId,
        /// How it ended.
        outcome: Outcome,
    },
    /// A request or an attempt was abandoned.
    Abandoned {
        /// Monotonic per-ledger sequence number.
        seq: u64,
        /// Wall-clock post time.
        #[serde(with = "crate::rfc3339::system_time")]
        at: SystemTime,
        /// The request abandoned.
        request: RequestId,
        /// `None` when the request was abandoned before any attempt was
        /// reserved; `Some` when an attempt was running and its executor is
        /// gone — which does NOT mean nothing happened.
        attempt: Option<AttemptId>,
        /// Why.
        reason: String,
    },
    /// A request's chain was voided (stale conditions, or 5 rejected
    /// credentials).
    Voided {
        /// Monotonic per-ledger sequence number.
        seq: u64,
        /// Wall-clock post time.
        #[serde(with = "crate::rfc3339::system_time")]
        at: SystemTime,
        /// The request voided.
        request: RequestId,
        /// Why.
        reason: String,
    },
    /// A standing grant was issued.
    StandingIssued {
        /// Monotonic per-ledger sequence number.
        seq: u64,
        /// Wall-clock post time.
        #[serde(with = "crate::rfc3339::system_time")]
        at: SystemTime,
        /// The standing grant issued.
        grant: StandingGrant,
    },
    /// A standing grant was revoked.
    StandingRevoked {
        /// Monotonic per-ledger sequence number.
        seq: u64,
        /// Wall-clock post time.
        #[serde(with = "crate::rfc3339::system_time")]
        at: SystemTime,
        /// The standing grant revoked.
        id: StandingId,
        /// Who revoked it.
        by: Principal,
        /// Why.
        reason: String,
    },
    /// A subscription was registered (spec §C.5).
    Subscribed {
        /// Monotonic per-ledger sequence number.
        seq: u64,
        /// Wall-clock post time.
        #[serde(with = "crate::rfc3339::system_time")]
        at: SystemTime,
        /// The subscription registered, carrying its allocated id.
        subscription: Subscription,
    },
    /// A subscription was revoked. Takes effect immediately for operations
    /// not yet posted; requests already granted under it are unaffected.
    Unsubscribed {
        /// Monotonic per-ledger sequence number.
        seq: u64,
        /// Wall-clock post time.
        #[serde(with = "crate::rfc3339::system_time")]
        at: SystemTime,
        /// The subscription revoked.
        id: SubscriptionId,
        /// Who revoked it.
        by: Principal,
        /// Why.
        reason: String,
    },
    /// A bad credential was presented.
    TokenRejected {
        /// Monotonic per-ledger sequence number.
        seq: u64,
        /// Wall-clock post time.
        #[serde(with = "crate::rfc3339::system_time")]
        at: SystemTime,
        /// `Some` when the presenting draft matched a live request (so the
        /// count means something); `None` when it matched nothing.
        request: Option<RequestId>,
        /// The running rejection count against `request`. The fifth
        /// rejection against one request voids it (spec §F.3).
        attempts: u32,
    },
}

impl LedgerEntry {
    /// This entry's monotonic per-ledger sequence number. Matched
    /// exhaustively *here*, inside the defining crate — a variant added
    /// without extending this match is a compile error, unlike a
    /// downstream `#[non_exhaustive]` match that would need a silent
    /// wildcard arm (spec §A.6's anti-drift template, applied to `seq`).
    pub fn seq(&self) -> u64 {
        match self {
            Self::Requested { seq, .. }
            | Self::Granted { seq, .. }
            | Self::Denied { seq, .. }
            | Self::Expired { seq, .. }
            | Self::KeyRetrieved { seq, .. }
            | Self::Redeemed { seq, .. }
            | Self::Refused { seq, .. }
            | Self::Settled { seq, .. }
            | Self::Abandoned { seq, .. }
            | Self::Voided { seq, .. }
            | Self::StandingIssued { seq, .. }
            | Self::StandingRevoked { seq, .. }
            | Self::Subscribed { seq, .. }
            | Self::Unsubscribed { seq, .. }
            | Self::TokenRejected { seq, .. } => *seq,
        }
    }
}

/// Test-only: a stamped, tokenless view, for exercising the control-plane
/// `.approval` field on `ExecResult`/`ToolResult`/`JobInfo` without standing
/// up a live ledger. One builder shared by every module's tests so the shape
/// under test cannot drift between them.
#[cfg(test)]
pub(crate) fn sample_view(operation: &str, paths: &[&str]) -> ApprovalRequestView {
    let draft = ApprovalRequest::builder(operation)
        .risk(RiskClass::Irreversible)
        .reason("the fs.* enforce policy is on")
        .hint(format!("{operation} --confirm=<token> {}", paths.join(" ")));
    paths
        .iter()
        .fold(draft, |b, p| b.resource(Resource::plain("path", *p)))
        .build()
        .expect("a well-formed draft")
        .stamp(
            RequestId::new(0x0badcafe, 1),
            Principal::new("session", PrincipalKind::Agent),
            Capture::Exact(Invocation {
                tool: operation.split('.').next().unwrap_or(operation).to_string(),
                argv: paths.iter().map(|p| (*p).to_string()).collect(),
            }),
            RequestContext::default(),
            std::time::UNIX_EPOCH,
            Duration::from_secs(60),
            None,
        )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── RequestId ──

    #[test]
    fn request_id_renders_in_full_form_with_no_hyphen() {
        let id = RequestId::new(0x9c1a4f2e, 42);
        assert_eq!(id.as_str(), "req_9c1a4f2e_42");
        assert!(!id.as_str().contains('-'), "must contain no hyphen: {id}");
        assert_eq!(id.to_string(), "req_9c1a4f2e_42");
    }

    #[test]
    fn request_id_round_trips_through_parse() {
        let id = RequestId::new(0x00000001, 0);
        let parsed = RequestId::parse(id.as_str()).expect("full-form id parses");
        assert_eq!(parsed, id);
    }

    #[test]
    fn request_id_short_forms_are_rejected() {
        // Epoch alone, no seq.
        assert!(RequestId::parse("req_9c1a4f2e").is_err());
        // No prefix at all.
        assert!(RequestId::parse("9c1a4f2e_42").is_err());
        // Truncated epoch.
        assert!(RequestId::parse("req_9c1a_42").is_err());
        // Trailing underscore, empty seq.
        assert!(RequestId::parse("req_9c1a4f2e_").is_err());
        // Uppercase hex is not accepted — the format is lowercase only.
        assert!(RequestId::parse("req_9C1A4F2E_42").is_err());
        // A hyphen anywhere is rejected outright — the format has none.
        assert!(RequestId::parse("req-9c1a4f2e-42").is_err());
        // Non-decimal seq.
        assert!(RequestId::parse("req_9c1a4f2e_4x").is_err());
    }

    #[test]
    fn request_id_serde_round_trips_and_rejects_short_forms() {
        let id = RequestId::new(0xdeadbeef, 7);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"req_deadbeef_7\"");
        let back: RequestId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);

        // A short form fails to deserialize, not just to parse().
        let bad: Result<RequestId, _> = serde_json::from_str("\"req_deadbeef\"");
        assert!(bad.is_err());
    }

    // ── Token ──

    #[test]
    fn token_debug_never_prints_the_raw_credential() {
        let raw = "a1b2c3d4e5f60718293a4b5c6d7e8f90";
        let token = Token::new(raw);
        let debug = format!("{token:?}");
        assert!(!debug.contains(raw), "Debug leaked the credential: {debug}");
        assert!(debug.contains("redacted"), "Debug should say redacted: {debug}");
        // The prefix IS allowed to appear — it's the correlation surface.
        assert!(debug.contains(&token.token_prefix()));
    }

    #[test]
    fn token_prefix_is_first_four_chars() {
        let token = Token::new("a1b2c3d4e5f6");
        assert_eq!(token.token_prefix(), "a1b2");
    }

    #[test]
    fn token_reveal_returns_the_raw_value() {
        let token = Token::new("deadbeef");
        assert_eq!(token.reveal(), "deadbeef");
    }

    // ── OperationId ──

    #[test]
    fn operation_id_namespaced_rejects_reserved_prefixes() {
        assert!(matches!(
            OperationId::namespaced("fs", "remove"),
            Err(OperationIdError::ReservedPrefix(p)) if p == "fs"
        ));
        assert!(matches!(
            OperationId::namespaced("trash", "empty"),
            Err(OperationIdError::ReservedPrefix(p)) if p == "trash"
        ));
    }

    #[test]
    fn operation_id_namespaced_allows_a_plugin_prefix() {
        let id = OperationId::namespaced("git", "push").unwrap();
        assert_eq!(id.as_str(), "git.push");
    }

    #[test]
    fn operation_id_namespaced_rejects_empty_parts() {
        assert!(OperationId::namespaced("", "push").is_err());
        assert!(OperationId::namespaced("git", "").is_err());
    }

    #[test]
    fn operation_id_new_rejects_empty() {
        assert!(matches!(OperationId::new(""), Err(OperationIdError::Empty)));
        assert!(OperationId::new("git.push").is_ok());
    }

    // ── StateClaim::Unspecified ──

    #[test]
    fn unspecified_never_equals_a_concrete_claim() {
        assert_ne!(StateClaim::Unspecified, StateClaim::Absent);
        assert_ne!(StateClaim::Unspecified, StateClaim::Exact("a1b2".to_string()));
        assert_ne!(
            StateClaim::Unspecified,
            StateClaim::Digest {
                alg: "sha256".to_string(),
                hex: "ff".to_string()
            }
        );
        // Only equals itself.
        assert_eq!(StateClaim::Unspecified, StateClaim::Unspecified);
    }

    // ── ApprovalRequest builder ──

    #[test]
    fn empty_operation_fails_to_build() {
        let err = ApprovalRequest::builder("")
            .risk(RiskClass::Reversible)
            .build()
            .unwrap_err();
        assert_eq!(err, ApprovalRequestBuildError::EmptyOperation);
    }

    #[test]
    fn missing_risk_fails_to_build() {
        let err = ApprovalRequest::builder("git.push").build().unwrap_err();
        assert_eq!(err, ApprovalRequestBuildError::MissingRisk);
    }

    #[test]
    fn builder_drafts_carry_no_principal_or_capture() {
        // Structural: ApprovalRequestDraft has no `principal` and no
        // `capture` field at all — this destructure is exhaustive (no `..`)
        // and would fail to compile if either field existed.
        let draft = ApprovalRequest::builder("git.push")
            .risk(RiskClass::Irreversible)
            .resource(Resource::plain("git.remote", "origin"))
            .reason("pushing to a protected branch")
            .hint("git push --confirm=<token> origin main")
            .build()
            .expect("valid draft");
        let ApprovalRequestDraft {
            operation,
            risk,
            resources,
            reason,
            hint,
            supersedes,
        } = draft;
        assert_eq!(operation.as_str(), "git.push");
        assert_eq!(risk, RiskClass::Irreversible);
        assert_eq!(resources.len(), 1);
        assert_eq!(reason, "pushing to a protected branch");
        assert!(hint.contains("<token>"));
        assert!(supersedes.is_none());
    }

    #[test]
    fn stamp_turns_a_draft_into_a_full_request() {
        let draft = ApprovalRequest::builder("git.push")
            .risk(RiskClass::Irreversible)
            .build()
            .unwrap();
        let req = draft.stamp(
            RequestId::new(1, 1),
            Principal::new("agent-1", PrincipalKind::Agent),
            Capture::DirectExecution,
            RequestContext::default(),
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(60),
            None,
        );
        assert_eq!(req.id.as_str(), "req_00000001_1");
        assert_eq!(req.principal.id, "agent-1");
        assert_eq!(req.capture, Capture::DirectExecution);
    }

    // ── GrantTerms ──

    #[test]
    fn once_for_copies_declared_transitions_into_conditions() {
        let draft = ApprovalRequest::builder("git.push")
            .risk(RiskClass::Irreversible)
            .resource(Resource::transition(
                "git.ref",
                "refs/heads/main",
                StateClaim::Exact("a1b2".to_string()),
                StateClaim::Exact("c3d4".to_string()),
            ))
            .resource(Resource::plain("git.remote", "origin"))
            .build()
            .unwrap();
        let req = draft.stamp(
            RequestId::new(1, 1),
            Principal::default(),
            Capture::DirectExecution,
            RequestContext::default(),
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(60),
            None,
        );
        let not_after = SystemTime::UNIX_EPOCH + Duration::from_secs(300);
        let terms = GrantTerms::once_for(&req, not_after);
        // Only the resource that declared a transition produces a condition.
        assert_eq!(terms.conditions.len(), 1);
        assert_eq!(terms.conditions[0].resource.kind, "git.ref");
        assert_eq!(terms.conditions[0].expected_from, StateClaim::Exact("a1b2".to_string()));
        assert_eq!(terms.not_after, not_after);
    }

    // ── Structural API-surface proofs: no Token field anywhere ──
    //
    // Token deliberately has no Serialize/Deserialize impl (see its doc
    // comment): that is the durable guarantee, because it means a struct in
    // this module that grew a `token: Token` field and derived Serialize
    // (every wide record here does) would fail to compile, not just fail a
    // test. The exhaustive destructures below are a second, narrower proof
    // for the specific structs the spec calls out — each list is closed (no
    // `..`), so an added field forces this test to be updated, and the type
    // ascriptions on the security-relevant fields catch a `Token` swap
    // directly.

    #[test]
    fn approval_request_has_no_credential_field() {
        let draft = ApprovalRequest::builder("git.push")
            .risk(RiskClass::Reversible)
            .build()
            .unwrap();
        let req = draft.stamp(
            RequestId::new(1, 1),
            Principal::default(),
            Capture::DirectExecution,
            RequestContext::default(),
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(60),
            None,
        );
        let ApprovalRequest {
            id,
            operation,
            risk,
            resources,
            principal,
            capture,
            context,
            job_id,
            reason,
            hint,
            requested_at,
            ttl,
            supersedes,
        } = req;
        let _: RequestId = id;
        let _: OperationId = operation;
        let _: RiskClass = risk;
        let _: Vec<Resource> = resources;
        let _: Principal = principal;
        let _: Capture = capture;
        let _: RequestContext = context;
        let _: Option<u64> = job_id;
        let _: String = reason;
        let _: String = hint;
        let _: SystemTime = requested_at;
        let _: Duration = ttl;
        let _: Option<RequestId> = supersedes;
    }

    #[test]
    fn approval_request_view_has_no_credential_field() {
        let draft = ApprovalRequest::builder("git.push")
            .risk(RiskClass::Reversible)
            .build()
            .unwrap();
        let req = draft.stamp(
            RequestId::new(1, 1),
            Principal::default(),
            Capture::DirectExecution,
            RequestContext::default(),
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(60),
            None,
        );
        let view: ApprovalRequestView = req.into();
        let ApprovalRequestView {
            id,
            operation,
            risk,
            resources,
            principal,
            capture,
            context,
            job_id,
            reason,
            hint,
            requested_at,
            ttl,
            supersedes,
        } = view;
        let _: RequestId = id;
        let _: OperationId = operation;
        let _: RiskClass = risk;
        let _: Vec<Resource> = resources;
        let _: Principal = principal;
        let _: Capture = capture;
        let _: RequestContext = context;
        let _: Option<u64> = job_id;
        let _: String = reason;
        let _: String = hint;
        let _: SystemTime = requested_at;
        let _: Duration = ttl;
        let _: Option<RequestId> = supersedes;
    }

    #[test]
    fn grant_has_no_redemption_limit_field() {
        let grant = Grant {
            request: RequestId::new(1, 1),
            decided_by: Principal::default(),
            grounds: Grounds::Embedder,
            not_after: SystemTime::UNIX_EPOCH,
            token_prefix: "a1b2".to_string(),
            conditions: Vec::new(),
            decided_at: SystemTime::UNIX_EPOCH,
        };
        // Exhaustive destructure: the single-success rule is structural —
        // there is no field here to configure a redemption count.
        let Grant {
            request,
            decided_by,
            grounds,
            not_after,
            token_prefix,
            conditions,
            decided_at,
        } = grant;
        let _: RequestId = request;
        let _: Principal = decided_by;
        let _: Grounds = grounds;
        let _: SystemTime = not_after;
        let _: String = token_prefix;
        let _: Vec<Condition> = conditions;
        let _: SystemTime = decided_at;
    }

    // ── serde round-trip: every LedgerEntry variant, including the tag ──

    fn sample_request() -> ApprovalRequest {
        ApprovalRequest::builder("git.push")
            .risk(RiskClass::Irreversible)
            .resource(Resource::transition(
                "git.ref",
                "refs/heads/main",
                StateClaim::Exact("a1b2".to_string()),
                StateClaim::Exact("c3d4".to_string()),
            ))
            .reason("pushing to a protected branch")
            .hint("git push --confirm=<token> origin main")
            .build()
            .unwrap()
            .stamp(
                RequestId::new(1, 1),
                Principal::new("agent-1", PrincipalKind::Agent),
                Capture::Exact(Invocation {
                    tool: "git".to_string(),
                    argv: vec!["push".to_string(), "origin".to_string(), "main".to_string()],
                }),
                RequestContext::default(),
                SystemTime::UNIX_EPOCH,
                Duration::from_secs(60),
                None,
            )
    }

    fn sample_grant() -> Grant {
        Grant {
            request: RequestId::new(1, 1),
            decided_by: Principal::new("amy", PrincipalKind::Human),
            grounds: Grounds::Human {
                channel: "repl".to_string(),
            },
            not_after: SystemTime::UNIX_EPOCH + Duration::from_secs(300),
            token_prefix: "a1b2".to_string(),
            conditions: vec![Condition {
                resource: ResourceRef {
                    kind: "git.ref".to_string(),
                    id: "refs/heads/main".to_string(),
                },
                expected_from: StateClaim::Exact("a1b2".to_string()),
            }],
            decided_at: SystemTime::UNIX_EPOCH,
        }
    }

    fn sample_standing_grant() -> StandingGrant {
        StandingGrant {
            id: StandingId::new(1),
            operations: vec![OperationPattern::new("git.commit")],
            resources: vec![ResourcePattern::new("git.ref", "refs/heads/agent/*")],
            principal: None,
            max_uses: Some(10),
            expires_at: None,
            issued_by: Principal::new("amy", PrincipalKind::Human),
            reason: "trust agent branches".to_string(),
        }
    }

    #[test]
    fn standing_grant_missing_max_uses_on_the_wire_is_one_shot_not_unlimited() {
        let mut value = serde_json::to_value(sample_standing_grant()).unwrap();
        value.as_object_mut().unwrap().remove("max_uses");
        let parsed: StandingGrant = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.max_uses, Some(1));
    }

    #[test]
    fn standing_grant_explicit_null_max_uses_is_unlimited_not_the_default() {
        // The null-versus-omitted split is deliberate and this test pins it:
        // null is the wire spelling of an explicit unlimited, omission is
        // the one-shot default. A producer meaning "default" must omit.
        let mut value = serde_json::to_value(sample_standing_grant()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("max_uses".to_string(), serde_json::Value::Null);
        let parsed: StandingGrant = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.max_uses, None);
    }

    #[test]
    fn standing_grant_is_one_shot_by_default_and_widening_is_explicit() {
        let base = || {
            StandingGrant::new(
                vec![OperationPattern::new("git.commit")],
                Vec::new(),
                None,
                None,
                Principal::new("amy", PrincipalKind::Human),
                "one-shot unless widened",
            )
        };
        assert_eq!(base().max_uses, Some(1));
        assert_eq!(base().with_max_uses(5).max_uses, Some(5));
        assert_eq!(base().unlimited_uses().max_uses, None);
    }

    fn all_entries() -> Vec<LedgerEntry> {
        let at = SystemTime::UNIX_EPOCH;
        let request = RequestId::new(1, 1);
        let by = Principal::new("amy", PrincipalKind::Human);
        vec![
            LedgerEntry::Requested {
                seq: 1,
                at,
                request: sample_request(),
            },
            LedgerEntry::Granted {
                seq: 2,
                at,
                grant: sample_grant(),
            },
            LedgerEntry::Denied {
                seq: 3,
                at,
                request: request.clone(),
                by: by.clone(),
                reason: "no".to_string(),
            },
            LedgerEntry::Expired {
                seq: 4,
                at,
                request: request.clone(),
                what: Expiring::Request,
            },
            LedgerEntry::KeyRetrieved {
                seq: 5,
                at,
                request: request.clone(),
                by: by.clone(),
            },
            LedgerEntry::Redeemed {
                seq: 6,
                at,
                request: request.clone(),
                attempt: AttemptId::new(1),
                by: by.clone(),
                observed: vec![Observation {
                    resource: ResourceRef {
                        kind: "git.ref".to_string(),
                        id: "refs/heads/main".to_string(),
                    },
                    claim: StateClaim::Exact("a1b2".to_string()),
                    at,
                }],
            },
            LedgerEntry::Refused {
                seq: 7,
                at,
                request: request.clone(),
                condition: Condition {
                    resource: ResourceRef {
                        kind: "git.ref".to_string(),
                        id: "refs/heads/main".to_string(),
                    },
                    expected_from: StateClaim::Exact("a1b2".to_string()),
                },
                found: StateClaim::Exact("e5f6".to_string()),
            },
            LedgerEntry::Settled {
                seq: 8,
                at,
                request: request.clone(),
                attempt: AttemptId::new(1),
                outcome: Outcome::Exit(0),
            },
            LedgerEntry::Abandoned {
                seq: 9,
                at,
                request: request.clone(),
                attempt: Some(AttemptId::new(2)),
                reason: "process exited mid-attempt".to_string(),
            },
            LedgerEntry::Voided {
                seq: 10,
                at,
                request: request.clone(),
                reason: "5 rejected credentials".to_string(),
            },
            LedgerEntry::StandingIssued {
                seq: 11,
                at,
                grant: sample_standing_grant(),
            },
            LedgerEntry::StandingRevoked {
                seq: 12,
                at,
                id: StandingId::new(1),
                by: by.clone(),
                reason: "policy changed".to_string(),
            },
            LedgerEntry::TokenRejected {
                seq: 13,
                at,
                request: Some(request.clone()),
                attempts: 3,
            },
        ]
    }

    #[test]
    fn request_level_abandoned_and_bogus_token_round_trip() {
        // The two Option-None shapes the main fixture doesn't cover: a request
        // abandoned before any attempt was reserved, and a bad key that
        // matched no live request at all.
        let at = SystemTime::UNIX_EPOCH;
        for entry in [
            LedgerEntry::Abandoned {
                seq: 1,
                at,
                request: RequestId::new(0x9c1a4f2e, 7),
                attempt: None,
                reason: "session shutdown before decision".to_string(),
            },
            LedgerEntry::TokenRejected {
                seq: 2,
                at,
                request: None,
                attempts: 1,
            },
        ] {
            let json = serde_json::to_value(&entry).expect("serialize");
            let back: LedgerEntry = serde_json::from_value(json).expect("deserialize");
            assert_eq!(entry, back);
        }
    }

    #[test]
    fn ledger_entry_timestamps_serialize_as_rfc3339_utc_strings() {
        // Same wire convention JobInfo pinned in kaish PR #273: every
        // SystemTime on the serde surface is an RFC 3339 UTC string.
        let entry = LedgerEntry::KeyRetrieved {
            seq: 1,
            at: SystemTime::UNIX_EPOCH,
            request: RequestId::new(0x9c1a4f2e, 7),
            by: Principal::new("amy", PrincipalKind::Human),
        };
        let json = serde_json::to_value(&entry).expect("serialize");
        assert_eq!(
            json.get("at").and_then(|v| v.as_str()),
            Some("1970-01-01T00:00:00.000Z"),
            "at must be an RFC 3339 string, got: {json}"
        );
    }

    #[test]
    fn request_id_parse_canonicalizes_leading_zero_seq() {
        let id = RequestId::parse("req_9c1a4f2e_042").expect("valid full form");
        assert_eq!(id.as_str(), "req_9c1a4f2e_42");
        assert_eq!(id, RequestId::parse("req_9c1a4f2e_42").expect("canonical"));
    }

    #[test]
    fn decision_wire_spellings_are_snake_case() {
        let deny = Decision::Deny {
            reason: "nope".to_string(),
        };
        let json = serde_json::to_value(&deny).expect("serialize");
        assert!(json.get("deny").is_some(), "expected snake_case tag: {json}");
        let defer = serde_json::to_value(Decision::Defer).expect("serialize");
        assert_eq!(defer, serde_json::json!("defer"));
    }

    const EXPECTED_TAGS: &[&str] = &[
        "requested",
        "granted",
        "denied",
        "expired",
        "key_retrieved",
        "redeemed",
        "refused",
        "settled",
        "abandoned",
        "voided",
        "standing_issued",
        "standing_revoked",
        "token_rejected",
    ];

    #[test]
    fn every_ledger_entry_variant_round_trips_with_its_tag() {
        let entries = all_entries();
        assert_eq!(
            entries.len(),
            EXPECTED_TAGS.len(),
            "every LedgerEntry variant must have a sample here"
        );
        for (entry, expected_tag) in entries.into_iter().zip(EXPECTED_TAGS) {
            let json = serde_json::to_value(&entry).unwrap();
            assert_eq!(
                json.get("entry").and_then(|v| v.as_str()),
                Some(*expected_tag),
                "wrong tag for {entry:?}: {json}"
            );
            let back: LedgerEntry = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(back, entry, "round-trip mismatch: {json}");
        }
    }

    #[test]
    fn ledger_entry_by_field_present_on_redeemed_and_key_retrieved() {
        let entries = all_entries();
        for entry in &entries {
            let json = serde_json::to_value(entry).unwrap();
            match entry {
                LedgerEntry::KeyRetrieved { .. } | LedgerEntry::Redeemed { .. } => {
                    assert!(json.get("by").is_some(), "{json} must carry `by`");
                }
                _ => {}
            }
        }
    }

    // ── Outcome: no plain Cancelled variant ──

    #[test]
    fn outcome_unknown_carries_a_lost_cause_not_a_bare_cancelled() {
        let outcome = Outcome::Unknown {
            cause: LostCause::Cancelled,
        };
        let json = serde_json::to_value(&outcome).unwrap();
        assert_eq!(json["unknown"]["cause"], "cancelled");
        // Confirms there is no bare "cancelled" outcome sitting next to
        // Exit/Error — every lost-executor outcome carries a cause.
        assert!(json.get("cancelled").is_none());
        let back: Outcome = serde_json::from_value(json).unwrap();
        assert_eq!(back, outcome);
    }

    // ── Principal / PrincipalKind defaults ──

    #[test]
    fn principal_defaults_to_unknown_kind() {
        assert_eq!(Principal::default().kind, PrincipalKind::Unknown);
    }
}
