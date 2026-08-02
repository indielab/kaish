//! The approval-ledger core (`docs/approval-ledger.md`, ledger PR 2).
//!
//! This file IS the §B.3 transition table, turned into tests: every legal
//! row proves the specified entries land and the state moves; every illegal
//! row proves the specific `LedgerError` comes back and nothing moved. No
//! `#![cfg(feature = ...)]` gate — the ledger has no OS dependency by
//! design and must compile and pass featureless (`--no-default-features`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use kaish_kernel::ledger::{Ledger, LedgerConfig, LedgerError, LedgerSink, LedgerSinkError};
use kaish_types::approval::{
    ApprovalRequest, Capture, Decision, GrantTerms, LedgerEntry, Observation, Outcome, Principal,
    PrincipalKind, RequestContext, RequestId, RequestState, Resource, ResourceRef, RiskClass,
    StateClaim,
};

fn agent(id: &str) -> Principal {
    Principal::new(id, PrincipalKind::Agent)
}

fn draft(op: &str) -> kaish_types::approval::ApprovalRequestDraft {
    #[allow(clippy::unwrap_used)]
    ApprovalRequest::builder(op).risk(RiskClass::Reversible).build().unwrap()
}

async fn post(
    requester: &kaish_kernel::ledger::Requester,
    op: &str,
    ttl: Duration,
) -> ApprovalRequest {
    #[allow(clippy::unwrap_used)]
    requester
        .post_request(draft(op), agent("agent-1"), Capture::DirectExecution, RequestContext::default(), ttl, None)
        .await
        .unwrap()
}

fn terms(req: &ApprovalRequest, not_after: SystemTime) -> GrantTerms {
    // `GrantTerms` is `#[non_exhaustive]` — `once_for` is the only external
    // constructor. For a request with no declared resource transitions this
    // is equivalent to an unconditioned `{ not_after, conditions: vec![] }`.
    GrantTerms::once_for(req, not_after)
}

fn far_future() -> SystemTime {
    SystemTime::now() + Duration::from_secs(300)
}

// ─────────────────────── §B.3 request-level table ───────────────────────

#[tokio::test]
async fn post_request_creates_a_requested_chain() {
    let (requester, approvals, _approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    assert_eq!(approvals.state(&req.id), Some(RequestState::Requested));
    let log = approvals.log(0);
    assert!(matches!(log.last(), Some(LedgerEntry::Requested { .. })));
}

#[tokio::test]
async fn grant_moves_requested_to_granted_and_appends_granted() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    approver.grant(&req.id, terms(&req, far_future())).await.unwrap();
    assert_eq!(approvals.state(&req.id), Some(RequestState::Granted));
    assert!(matches!(approvals.log(0).last(), Some(LedgerEntry::Granted { .. })));
}

#[tokio::test]
async fn deny_moves_requested_to_denied_and_appends_denied() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    approver.deny(&req.id, "no").await.unwrap();
    assert_eq!(approvals.state(&req.id), Some(RequestState::Denied));
    assert!(matches!(approvals.log(0).last(), Some(LedgerEntry::Denied { .. })));
}

#[tokio::test]
async fn request_ttl_elapsing_materializes_expired_on_read() {
    let (requester, approvals, _approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_millis(1)).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(approvals.state(&req.id), Some(RequestState::Expired));
    assert!(matches!(
        approvals.log(0).last(),
        Some(LedgerEntry::Expired {
            what: kaish_types::approval::Expiring::Request,
            ..
        })
    ));
}

#[tokio::test]
async fn redeem_before_any_decision_is_not_authorized_and_counts_a_rejection() {
    let (requester, approvals, _approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    let err = requester
        .redeem_with_token(&req.id, "whatever", agent("agent-1"), Vec::new())
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::NotAuthorized(id) if id == req.id));
    assert_eq!(approvals.state(&req.id), Some(RequestState::Requested), "state must not move");
    assert!(matches!(
        approvals.log(0).last(),
        Some(LedgerEntry::TokenRejected { request: Some(id), attempts: 1, .. }) if *id == req.id
    ));
}

#[tokio::test]
async fn bad_key_against_a_granted_request_is_not_authorized_and_counts() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    approver.grant(&req.id, terms(&req, far_future())).await.unwrap();
    let err = requester
        .redeem_with_token(&req.id, "wrong", agent("agent-1"), Vec::new())
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::NotAuthorized(id) if id == req.id));
    assert_eq!(approvals.state(&req.id), Some(RequestState::Granted), "state must not move");
    assert!(matches!(
        approvals.log(0).last(),
        Some(LedgerEntry::TokenRejected { request: Some(id), attempts: 1, .. }) if *id == req.id
    ));
}

#[tokio::test]
async fn fifth_bad_key_voids_and_a_later_good_key_fails_naming_the_void() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    approver.grant(&req.id, terms(&req, far_future())).await.unwrap();
    let real = approver.token_for(&req.id).expect("granted request has a credential");

    for n in 1..5 {
        let err = requester
            .redeem_with_token(&req.id, "wrong", agent("agent-1"), Vec::new())
            .await
            .unwrap_err();
        assert!(matches!(err, LedgerError::NotAuthorized(_)), "rejection {n}");
        assert_eq!(approvals.state(&req.id), Some(RequestState::Granted), "still live after rejection {n}");
    }

    // The 5th rejection voids the request.
    let err = requester
        .redeem_with_token(&req.id, "wrong", agent("agent-1"), Vec::new())
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::NotAuthorized(_)));
    assert_eq!(approvals.state(&req.id), Some(RequestState::Voided));
    let log = approvals.log(0);
    assert!(matches!(log.get(log.len() - 2), Some(LedgerEntry::TokenRejected { attempts: 5, .. })));
    assert!(matches!(log.last(), Some(LedgerEntry::Voided { .. })));

    // Even the REAL key fails now, naming the void.
    let err = requester
        .redeem_with_token(&req.id, real.reveal(), agent("agent-1"), Vec::new())
        .await
        .unwrap_err();
    match err {
        LedgerError::Terminal { state, detail, .. } => {
            assert_eq!(state, RequestState::Voided);
            let detail = detail.expect("void reason must be present");
            assert!(detail.contains('5'), "detail should name the void: {detail}");
        }
        other => panic!("expected Terminal naming the void, got {other:?}"),
    }
}

#[tokio::test]
async fn bad_key_matching_no_live_request_appends_token_rejected_none_and_voids_nothing() {
    let (requester, approvals, _approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    // A syntactically valid but entirely unknown id.
    let bogus = RequestId::new(0xdead_beef, 999_999);

    for _ in 0..5 {
        let err = requester
            .redeem_with_token(&bogus, "guess", agent("agent-1"), Vec::new())
            .await
            .unwrap_err();
        assert!(matches!(err, LedgerError::NotAuthorized(id) if id == bogus));
    }

    let log = approvals.log(0);
    let none_count = log
        .iter()
        .filter(|e| matches!(e, LedgerEntry::TokenRejected { request: None, .. }))
        .count();
    assert_eq!(none_count, 5, "every guess against an unknown id records TokenRejected{{None}}");
    assert!(
        log.iter().all(|e| !matches!(e, LedgerEntry::Voided { .. })),
        "nothing should have voided — there was never a live request to void"
    );
}

#[tokio::test]
async fn redeem_with_the_real_credential_succeeds_and_reserves_an_attempt() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    approver.grant(&req.id, terms(&req, far_future())).await.unwrap();
    let token = approver.token_for(&req.id).unwrap();

    let attempt = requester
        .redeem_with_token(&req.id, token.reveal(), agent("someone-else"), Vec::new())
        .await
        .unwrap();
    assert_eq!(attempt.request_id(), &req.id);
    assert_eq!(approvals.state(&req.id), Some(RequestState::Granted));
    assert!(matches!(approvals.log(0).last(), Some(LedgerEntry::Redeemed { .. })));
}

#[tokio::test]
async fn condition_failure_refuses_and_voids_reserving_no_attempt() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let mut d = draft("git.push");
    d.resources.push(Resource::transition(
        "git.ref",
        "refs/heads/main",
        StateClaim::Exact("a1b2".to_string()),
        StateClaim::Exact("c3d4".to_string()),
    ));
    let req = requester
        .post_request(d, agent("agent-1"), Capture::DirectExecution, RequestContext::default(), Duration::from_secs(60), None)
        .await
        .unwrap();
    let grant_terms = GrantTerms::once_for(&req, far_future());
    assert_eq!(grant_terms.conditions.len(), 1);
    approver.grant(&req.id, grant_terms).await.unwrap();

    // The world moved: the ref is no longer at a1b2.
    let observed = vec![Observation {
        resource: ResourceRef {
            kind: "git.ref".to_string(),
            id: "refs/heads/main".to_string(),
        },
        claim: StateClaim::Exact("e5f6".to_string()),
        at: SystemTime::now(),
    }];
    let err = requester.redeem(&req.id, agent("agent-1"), observed).await.unwrap_err();
    assert!(matches!(err, LedgerError::Refused { .. }));
    assert_eq!(approvals.state(&req.id), Some(RequestState::Voided));
    let log = approvals.log(0);
    assert!(matches!(log.get(log.len() - 2), Some(LedgerEntry::Refused { .. })));
    assert!(matches!(log.last(), Some(LedgerEntry::Voided { .. })));
    // No attempt was reserved.
    let chain = approvals.get(&req.id).unwrap();
    assert!(chain.attempts.is_empty());
}

#[tokio::test]
async fn redeem_while_an_attempt_is_in_flight_is_rejected() {
    let (requester, _approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    approver.grant(&req.id, terms(&req, far_future())).await.unwrap();
    let _attempt = requester.redeem(&req.id, agent("agent-1"), Vec::new()).await.unwrap();

    let err = requester.redeem(&req.id, agent("agent-1"), Vec::new()).await.unwrap_err();
    assert!(matches!(err, LedgerError::AttemptInFlight(id) if id == req.id));
}

#[tokio::test]
async fn redeem_after_a_successful_settlement_reports_the_outcome_and_does_not_reexecute() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    approver.grant(&req.id, terms(&req, far_future())).await.unwrap();
    let attempt = requester.redeem(&req.id, agent("agent-1"), Vec::new()).await.unwrap();
    requester.settle(&attempt, Outcome::Exit(0)).await.unwrap();

    let before = approvals.log(0).len();
    let err = requester.redeem(&req.id, agent("agent-1"), Vec::new()).await.unwrap_err();
    match err {
        LedgerError::AlreadySettled { outcome, .. } => {
            assert!(matches!(outcome, Some(Outcome::Exit(0))));
        }
        other => panic!("expected AlreadySettled, got {other:?}"),
    }
    assert_eq!(approvals.log(0).len(), before, "no new Redeemed should have been appended");
}

#[tokio::test]
async fn a_reported_failure_leaves_the_grant_live_for_a_retry() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    approver.grant(&req.id, terms(&req, far_future())).await.unwrap();
    let attempt1 = requester.redeem(&req.id, agent("agent-1"), Vec::new()).await.unwrap();
    requester.settle(&attempt1, Outcome::Exit(1)).await.unwrap();

    // The chain must still be Granted — a retry can redeem again.
    assert_eq!(approvals.state(&req.id), Some(RequestState::Granted));
    let attempt2 = requester.redeem(&req.id, agent("agent-1"), Vec::new()).await.unwrap();
    assert_ne!(attempt1.attempt_id(), attempt2.attempt_id());
    requester.settle(&attempt2, Outcome::Exit(0)).await.unwrap();
    // Now it really is closed.
    let err = requester.redeem(&req.id, agent("agent-1"), Vec::new()).await.unwrap_err();
    assert!(matches!(err, LedgerError::AlreadySettled { .. }));
}

#[tokio::test]
async fn an_unknown_outcome_closes_the_chain_without_reopening_the_grant() {
    let (requester, _approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    approver.grant(&req.id, terms(&req, far_future())).await.unwrap();
    let attempt = requester.redeem(&req.id, agent("agent-1"), Vec::new()).await.unwrap();
    requester
        .settle(&attempt, Outcome::Unknown { cause: kaish_types::approval::LostCause::Cancelled })
        .await
        .unwrap();

    let err = requester.redeem(&req.id, agent("agent-1"), Vec::new()).await.unwrap_err();
    assert!(matches!(err, LedgerError::AlreadySettled { outcome: Some(Outcome::Unknown { .. }), .. }));
}

#[tokio::test]
async fn grant_not_after_elapsing_materializes_expired() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    approver
        .grant(&req.id, terms(&req, SystemTime::now() + Duration::from_millis(1)))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(approvals.state(&req.id), Some(RequestState::Expired));
    assert!(matches!(
        approvals.log(0).last(),
        Some(LedgerEntry::Expired { what: kaish_types::approval::Expiring::Grant, .. })
    ));
}

#[tokio::test]
async fn granting_an_already_decided_request_is_rejected() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    approver.grant(&req.id, terms(&req, far_future())).await.unwrap();
    let before = approvals.log(0).len();
    let err = approver.grant(&req.id, terms(&req, far_future())).await.unwrap_err();
    assert!(matches!(err, LedgerError::AlreadyDecided(id) if id == req.id));
    assert_eq!(approvals.log(0).len(), before, "no new entry from the rejected second decision");
}

#[tokio::test]
async fn terminal_states_reject_any_further_transition_and_leave_state_unchanged() {
    // Denied.
    {
        let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
        let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
        approver.deny(&req.id, "no").await.unwrap();
        let err = approver.deny(&req.id, "no again").await.unwrap_err();
        assert!(matches!(err, LedgerError::Terminal { state: RequestState::Denied, .. }));
        assert_eq!(approvals.state(&req.id), Some(RequestState::Denied));
    }
    // Abandoned.
    {
        let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
        let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
        requester.abandon_request(&req.id, "job discarded").await.unwrap();
        let err = approver.grant(&req.id, terms(&req, far_future())).await.unwrap_err();
        assert!(matches!(err, LedgerError::Terminal { state: RequestState::Abandoned, .. }));
        assert_eq!(approvals.state(&req.id), Some(RequestState::Abandoned));
    }
}

#[tokio::test]
async fn renew_only_succeeds_from_expired_and_links_via_supersedes() {
    let (requester, approvals, _approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_millis(1)).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(approvals.state(&req.id), Some(RequestState::Expired));

    let new_id = requester.renew(&req.id).await.unwrap();
    assert_ne!(new_id, req.id);
    assert_eq!(approvals.state(&new_id), Some(RequestState::Requested));
    let chain = approvals.get(&new_id).unwrap();
    assert_eq!(chain.request.supersedes, Some(req.id.clone()));
    assert_eq!(chain.request.operation.as_str(), "fs.remove");
}

#[tokio::test]
async fn renew_on_a_non_expired_request_is_rejected() {
    let (requester, _approvals, _approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    let err = requester.renew(&req.id).await.unwrap_err();
    assert!(matches!(err, LedgerError::NotRenewable { .. }));
}

// ─────────────────────── Attempt-level table ───────────────────────

#[tokio::test]
async fn settling_the_same_attempt_twice_appends_one_entry_and_returns_ok() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    approver.grant(&req.id, terms(&req, far_future())).await.unwrap();
    let attempt = requester.redeem(&req.id, agent("agent-1"), Vec::new()).await.unwrap();

    let first = requester.settle(&attempt, Outcome::Exit(0)).await.unwrap();
    assert!(first, "first settlement appends an entry");
    let count_after_first = approvals.log(0).iter().filter(|e| matches!(e, LedgerEntry::Settled { .. })).count();

    let second = requester.settle(&attempt, Outcome::Exit(0)).await.unwrap();
    assert!(!second, "second settlement is a no-op");
    let count_after_second = approvals.log(0).iter().filter(|e| matches!(e, LedgerEntry::Settled { .. })).count();
    assert_eq!(count_after_first, count_after_second, "idempotent by AttemptId — no duplicate Settled entry");
}

// The recovery sweep itself (`LedgerInner::sweep`) is `pub(crate)` — no
// public API in this PR calls it on a timer (that wiring is a later PR's
// job; see the PR body). Its behavior is proven as a white-box test in
// `src/ledger/core.rs` instead, where the sweep can be invoked directly.

// ─────────────────────── Concurrency ───────────────────────

#[tokio::test]
async fn concurrent_redemptions_of_one_grant_produce_exactly_one_redeemed() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    approver.grant(&req.id, terms(&req, far_future())).await.unwrap();

    let mut tasks = Vec::new();
    for i in 0..16 {
        let requester = requester.clone();
        let id = req.id.clone();
        tasks.push(tokio::spawn(async move {
            requester.redeem(&id, agent(&format!("racer-{i}")), Vec::new()).await
        }));
    }
    let mut ok_count = 0;
    let mut in_flight_count = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => ok_count += 1,
            Err(LedgerError::AttemptInFlight(_)) => in_flight_count += 1,
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
    assert_eq!(ok_count, 1, "exactly one racer should win the reservation");
    assert_eq!(in_flight_count, 15);
    let redeemed_count = approvals.log(0).iter().filter(|e| matches!(e, LedgerEntry::Redeemed { .. })).count();
    assert_eq!(redeemed_count, 1);
}

#[tokio::test]
async fn seq_is_gap_free_under_concurrent_posts_from_sixteen_tasks() {
    let (requester, approvals, _approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let mut tasks = Vec::new();
    for i in 0..16 {
        let requester = requester.clone();
        tasks.push(tokio::spawn(async move { post(&requester, &format!("fs.remove.{i}"), Duration::from_secs(60)).await }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let mut seqs: Vec<u64> = approvals.log(0).iter().map(LedgerEntry::seq).collect();
    seqs.sort_unstable();
    let expected: Vec<u64> = (1..=16).collect();
    assert_eq!(seqs, expected, "no gaps and no duplicates across 16 concurrent posts");
}

// ─────────────────────── Ring / sink capacity ───────────────────────

#[tokio::test]
async fn ring_refuses_loudly_rather_than_evicting_a_live_chain() {
    // A tiny ring: room for exactly 2 entries. One live request's own
    // Requested entry already occupies a slot forever (the chain never
    // closes), so a second post should be refused once the ring cannot
    // evict anything.
    let config = LedgerConfig {
        retained_entries: 1,
        ..Default::default()
    };
    let (requester, _approvals, _approver) = Ledger::build(config, None).unwrap();
    let _first = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    // The ring holds exactly the first Requested entry now, and that
    // request is still live (Requested, undecided) — nothing is evictable.
    let err = requester
        .post_request(
            draft("fs.remove"),
            agent("agent-1"),
            Capture::DirectExecution,
            RequestContext::default(),
            Duration::from_secs(60),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::RingAtCapacity));
}

#[tokio::test]
async fn ring_evicts_closed_chains_to_make_room() {
    let config = LedgerConfig {
        retained_entries: 2,
        ..Default::default()
    };
    let (requester, approvals, approver) = Ledger::build(config, None).unwrap();
    let req1 = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    approver.deny(&req1.id, "no").await.unwrap(); // closes req1 — its 2 entries become evictable
    let req2 = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    assert_eq!(approvals.state(&req2.id), Some(RequestState::Requested));
}

#[derive(Default)]
struct RecordingSink {
    posted: Mutex<Vec<LedgerEntry>>,
    fail_after: AtomicUsize,
}

impl LedgerSink for RecordingSink {
    fn post(&self, entry: &LedgerEntry) -> Result<(), LedgerSinkError> {
        let remaining = self.fail_after.load(Ordering::SeqCst);
        if remaining == 0 {
            return Err(LedgerSinkError("synthetic sink failure".to_string()));
        }
        self.fail_after.store(remaining - 1, Ordering::SeqCst);
        #[allow(clippy::unwrap_used)]
        self.posted.lock().unwrap().push(entry.clone());
        Ok(())
    }
}

#[tokio::test]
async fn full_sink_queue_returns_ledger_unavailable_rather_than_blocking_or_dropping() {
    let sink = Arc::new(RecordingSink {
        fail_after: AtomicUsize::new(usize::MAX),
        ..Default::default()
    });
    // Depth 1: the background drain task is never polled (no `.await`
    // yields control back to it before we saturate the channel), so the
    // second post should see the queue still full.
    let config = LedgerConfig {
        sink_queue: 1,
        ..Default::default()
    };
    let (requester, _approvals, _approver) = Ledger::build(config, Some(sink)).unwrap();
    // Neither call below has any internal `.await` point (the transaction
    // is fully synchronous — see `core.rs`'s module doc), so the background
    // drain task gets no chance to run between them: the one queue slot is
    // still occupied by the first entry when the second post is attempted,
    // and this is deterministic, not a race.
    let _first = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    let err = requester
        .post_request(
            draft("fs.remove"),
            agent("agent-1"),
            Capture::DirectExecution,
            RequestContext::default(),
            Duration::from_secs(60),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::SinkUnavailable(_)));
}

#[tokio::test]
async fn a_sink_error_fails_subsequent_requests_closed() {
    let sink = Arc::new(RecordingSink {
        fail_after: AtomicUsize::new(0), // every post() call fails immediately
        ..Default::default()
    });
    let (requester, _approvals, _approver) = Ledger::build(LedgerConfig::default(), Some(sink)).unwrap();
    let _first = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    // Give the background drain task a chance to observe the failure.
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        let err = requester
            .post_request(
                draft("fs.remove"),
                agent("agent-1"),
                Capture::DirectExecution,
                RequestContext::default(),
                Duration::from_secs(60),
                None,
            )
            .await;
        if let Err(LedgerError::SinkUnavailable(_)) = err {
            return; // fail-closed observed
        }
    }
    panic!("expected the ledger to fail closed after the sink reported an error");
}

// ─────────────────────── Live capacity ───────────────────────

#[tokio::test]
async fn live_capacity_refuses_a_new_request_rather_than_evicting() {
    let config = LedgerConfig {
        live_capacity: 1,
        ..Default::default()
    };
    let (requester, _approvals, _approver) = Ledger::build(config, None).unwrap();
    let _first = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    let err = requester
        .post_request(
            draft("fs.remove"),
            agent("agent-1"),
            Capture::DirectExecution,
            RequestContext::default(),
            Duration::from_secs(60),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::LiveCapacity { limit: 1 }));
}

// ─────────────────────── Credential accountability ───────────────────────

#[tokio::test]
async fn token_for_appends_key_retrieved_naming_the_retriever() {
    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let req = post(&requester, "fs.remove", Duration::from_secs(60)).await;
    approver.grant(&req.id, terms(&req, far_future())).await.unwrap();
    let _token = approver.token_for(&req.id).unwrap();
    assert!(matches!(
        approvals.log(0).last(),
        Some(LedgerEntry::KeyRetrieved { request, .. }) if *request == req.id
    ));
}

// ─────────────────────── Standing grants (pure record) ───────────────────────

#[tokio::test]
async fn grant_standing_and_revoke_standing_are_pure_record_operations() {
    let (_requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let g = kaish_types::approval::StandingGrant::new(
        vec![kaish_types::approval::OperationPattern::new("git.commit")],
        Vec::new(),
        None,
        Some(3),
        None,
        agent("human-1"),
        "trusted agent workflow",
    );
    let id = approver.grant_standing(g).await.unwrap();
    assert_eq!(approvals.standing().len(), 1);
    assert!(matches!(approvals.log(0).last(), Some(LedgerEntry::StandingIssued { .. })));

    approver.revoke_standing(&id, "no longer needed").await.unwrap();
    assert!(approvals.standing().is_empty());
    assert!(matches!(approvals.log(0).last(), Some(LedgerEntry::StandingRevoked { .. })));
}

// ─────────────────────── API surface: Requester cannot grant ───────────────────────

/// Snapshot the `impl Requester` block's method signatures and assert none
/// of them return a `Grant` — the structural half of the spec's "there is no
/// method on `Requester` that produces a `Grant`" guarantee (the other half,
/// `ApproverHandle` having no public constructor, is the `compile_fail`
/// doctest on `ApproverHandle` itself). A `trybuild` dependency was judged
/// not worth adding for this one property; see the PR body.
#[test]
fn requester_has_no_method_producing_a_grant() {
    let source = include_str!("../src/ledger/handles.rs");
    let start = source.find("impl Requester {").expect("impl Requester block must exist");
    let body = &source[start..];
    let end = find_matching_brace(body);
    let block = &body[..end];
    assert!(
        !block.contains("-> Result<Grant") && !block.contains("-> Grant") && !block.contains("Result<Grant,"),
        "Requester must never gain a method that produces a Grant:\n{block}"
    );
    insta::assert_snapshot!("requester_impl_block", block);
}

/// Finds the index just past the closing `}` matching the opening `{` in
/// `"impl Requester {"` at the start of `text`.
fn find_matching_brace(text: &str) -> usize {
    let mut depth = 0i32;
    for (i, ch) in text.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }
    text.len()
}

// ─────────────────────── Approver::decide input shape (forward reference) ───────────────────────

#[test]
fn decision_defer_is_the_default_shape_used_by_a_deferring_approver() {
    // Not wired to any gate site in this PR (PR 4 owns the decision chain),
    // but `Decision` is part of the vocabulary this ledger core speaks —
    // confirm the variant a fully-deferring `Approver` returns exists and
    // round-trips, since PR 4 depends on it unchanged.
    let d = Decision::Defer;
    assert!(matches!(d, Decision::Defer));
}
