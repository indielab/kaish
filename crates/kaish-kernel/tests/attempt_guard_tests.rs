//! The dispatcher's drop-safe `AttemptGuard` (`docs/approval-ledger.md` §C.1,
//! ledger PR 3).
//!
//! Nothing in production constructs one of these yet — no gate site calls
//! `ToolCtx::request_approval` (that begins at the PR 5 cutover) — so these
//! tests build the guard directly against a real ledger, the same way
//! `ledger_core_tests.rs` exercises the ledger's own handles. No
//! `#![cfg(feature = ...)]` gate: the ledger and its guard have no OS
//! dependency and must compile and pass featureless.

use std::time::{Duration, SystemTime};

use kaish_kernel::ledger::{AttemptGuard, Ledger, LedgerConfig};
use kaish_types::approval::{
    ApprovalRequest, Capture, GrantTerms, LostCause, Outcome, Principal, PrincipalKind,
    RequestContext, RequestState,
};

fn agent(id: &str) -> Principal {
    Principal::new(id, PrincipalKind::Agent)
}

fn draft(op: &str) -> kaish_types::approval::ApprovalRequestDraft {
    #[allow(clippy::unwrap_used)]
    ApprovalRequest::builder(op)
        .risk(kaish_types::approval::RiskClass::Reversible)
        .build()
        .unwrap()
}

fn far_future() -> SystemTime {
    SystemTime::now() + Duration::from_secs(300)
}

/// Drains the outbox without depending on any `pub(crate)` internal:
/// `Approvals::pending()` runs the full sweep, and PR 3 wires the sweep to
/// drain the outbox first (spec §C.1).
fn force_drain(approvals: &kaish_kernel::ledger::Approvals) {
    let _ = approvals.pending();
}

#[tokio::test]
async fn dropped_attempt_guard_settles_as_unknown_cancelled_never_an_exit_code() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = requester
        .post_request(draft("plugin.dangerous"), agent("agent-1"), Capture::DirectExecution, RequestContext::default(), Duration::from_secs(60), None)
        .await
        .unwrap();
    approver.grant(&req.id, GrantTerms::once_for(&req, far_future())).await.unwrap();
    let attempt = requester.redeem(&req.id, agent("agent-1"), Vec::new()).await.unwrap();

    // The dispatcher's shape: build the guard, then the tool's future is
    // dropped before it ever reports an outcome (cancellation, task abort).
    let guard = AttemptGuard::new(requester.clone(), attempt);
    drop(guard);

    force_drain(&approvals);

    let chain = approvals.get(&req.id).expect("chain must still exist");
    assert_eq!(chain.attempts.len(), 1, "exactly one attempt was reserved");
    let settled = &chain.attempts[0];
    assert!(
        matches!(settled.outcome, Some(Outcome::Unknown { cause: LostCause::Cancelled })),
        "a dropped guard must settle Unknown{{Cancelled}}, got {:?}",
        settled.outcome
    );
    assert!(
        !matches!(settled.outcome, Some(Outcome::Exit(_))),
        "a dropped guard must never settle as an exit code"
    );
    // Unknown closes the chain (spec §B.2) — it stays nominally `Granted`
    // (there is no separate "closed" state) but is not reservable again.
    assert_eq!(approvals.state(&req.id), Some(RequestState::Granted));
    let err = requester.redeem(&req.id, agent("agent-1"), Vec::new()).await.unwrap_err();
    assert!(
        matches!(err, kaish_kernel::ledger::LedgerError::AlreadySettled { .. }),
        "a closed chain must refuse a second reservation, got {err:?}"
    );
}

#[tokio::test]
async fn panicking_tool_future_settles_the_same_way_as_a_drop() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = requester
        .post_request(draft("plugin.dangerous"), agent("agent-1"), Capture::DirectExecution, RequestContext::default(), Duration::from_secs(60), None)
        .await
        .unwrap();
    approver.grant(&req.id, GrantTerms::once_for(&req, far_future())).await.unwrap();
    let attempt = requester.redeem(&req.id, agent("agent-1"), Vec::new()).await.unwrap();

    let task_requester = requester.clone();
    let join = tokio::spawn(async move {
        let _guard = AttemptGuard::new(task_requester, attempt);
        // Rust unwinds through `_guard`'s `Drop` on the way out of this
        // frame, the same as a real tool panicking mid-`execute()`.
        panic!("simulated tool panic mid-execution");
    });
    let panicked = join.await;
    assert!(panicked.is_err(), "the spawned task must have panicked");

    force_drain(&approvals);

    let chain = approvals.get(&req.id).expect("chain must still exist");
    let settled = &chain.attempts[0];
    assert!(
        matches!(settled.outcome, Some(Outcome::Unknown { cause: LostCause::Cancelled })),
        "a panicking tool must settle Unknown{{Cancelled}} via the guard's Drop, got {:?}",
        settled.outcome
    );
}

#[tokio::test]
async fn explicit_settle_before_drop_wins_and_the_drop_push_is_a_no_op() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = requester
        .post_request(draft("plugin.dangerous"), agent("agent-1"), Capture::DirectExecution, RequestContext::default(), Duration::from_secs(60), None)
        .await
        .unwrap();
    approver.grant(&req.id, GrantTerms::once_for(&req, far_future())).await.unwrap();
    let attempt = requester.redeem(&req.id, agent("agent-1"), Vec::new()).await.unwrap();

    let guard = AttemptGuard::new(requester.clone(), attempt);
    // The dispatcher's normal-return path: settle with the real outcome
    // before the guard drops.
    let appended = guard.settle(Outcome::Exit(0)).await.unwrap();
    assert!(appended);
    drop(guard);
    force_drain(&approvals);

    let chain = approvals.get(&req.id).expect("chain must still exist");
    let settled = &chain.attempts[0];
    assert_eq!(
        settled.outcome,
        Some(Outcome::Exit(0)),
        "the explicit settle must win — the guard's later Drop push must be an idempotent no-op"
    );
}
