//! The approval ledger (`docs/approval-ledger.md`, ledger PRs 2 and 4).
//!
//! One append-only log, two posting authorities, and a linearization
//! contract enforced by a single lock (spec §A.1, §B.1). This module is the
//! state machine: [`Ledger::build`] mints a [`Requester`] (obligations —
//! `Requested`/`Redeemed`/`Settled`), an [`Approvals`] (read-only), and one
//! [`ApproverHandle`] (authorizations — `Granted`/`Denied`/standing grants/
//! credential retrieval). There is no method on `Requester` that produces a
//! `Grant`; that split, enforced by the type system, is the whole point.
//!
//! **What is here:** both state machines (spec §B.2–§B.3), the credential
//! index and its rejected-attempt voiding (§F.3), idempotent settlement
//! (§A.1), partitioned retention with a ring that refuses to evict a live
//! chain (§D.4), sink backpressure, the invariant checks, the recovery
//! sweep, and — from PR 4 — the four-stage [`DecisionChain`] (standing →
//! `policy` → `decide` → defer) with standing-grant matching (§C.2, §C.4).
//!
//! **What is not:** this module is wired to no gate site. Nothing in
//! `kaish-kernel`'s dispatch path calls into it yet, and building it changed
//! no observable behavior anywhere else in the crate. `ToolCtx` gained its
//! `request_approval`/`approvals`/`settle_with` methods and `ExecContext`
//! its real implementations in PR 3, alongside this module's
//! [`AttemptGuard`]; `Kernel::build` constructs a ledger and a
//! [`DecisionChain`] (PR 4) — but no gate site calls either yet.
//! `Kernel::confirm` and the ten gate sites (PR 5, the cutover) are what
//! give them callers.

mod approver;
mod attempt_guard;
mod config;
mod core;
mod error;
mod handles;
mod operation;
mod resolver;
mod standing;

pub use approver::{
    Approver, ChainContext, ChainOutcome, ChainStage, DecisionChain, PatientSource,
    DEFAULT_DECIDE_BUDGET,
};
pub use attempt_guard::AttemptGuard;
pub use config::{LedgerConfig, LedgerSink, LedgerSinkError};
pub use error::LedgerError;
pub use operation::KernelOperation;
pub use resolver::{
    ConditionReport, PathResolver, ResolverError, StateResolver, StateResolverConflict,
    StateResolvers, PATH_DIGEST_ALG, PATH_KIND,
};
pub(crate) use resolver::{conditions_to_observe, digest_path};
pub use handles::{ApproverHandle, AttemptHandle, AttemptView, Approvals, Ledger, RequestChain, Requester};

/// Test-only: a stamped, tokenless view for exercising the control-plane
/// `.approval` field (job rows, scatter rows, pipeline overrides) without
/// standing up a live ledger. In-crate unit tests only — an integration test
/// gets a real view from a real ledger.
#[cfg(test)]
pub(crate) fn sample_view(
    operation: KernelOperation,
    paths: &[&str],
) -> kaish_types::approval::ApprovalRequestView {
    use kaish_types::approval::{
        ApprovalRequest, Capture, Invocation, Principal, PrincipalKind, RequestContext, RequestId,
        Resource,
    };
    let draft = ApprovalRequest::builder(operation.as_str())
        .risk(operation.risk())
        .reason("the fs.* enforce policy is on")
        .hint(format!("rm --confirm=<token> {}", paths.join(" ")));
    paths
        .iter()
        .fold(draft, |b, p| b.resource(Resource::plain("path", *p)))
        .build()
        .expect("a well-formed draft")
        .stamp(
            RequestId::new(0x0badcafe, 1),
            Principal::new("session", PrincipalKind::Agent),
            Capture::Exact(Invocation {
                tool: "rm".to_string(),
                argv: paths.iter().map(|p| (*p).to_string()).collect(),
            }),
            RequestContext::default(),
            std::time::UNIX_EPOCH,
            std::time::Duration::from_secs(60),
            None,
        )
        .into()
}
