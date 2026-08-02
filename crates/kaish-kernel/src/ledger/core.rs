//! The ledger's one critical section (`docs/approval-ledger.md` §B.1).
//!
//! [`LedgerInner`] holds one `std::sync::Mutex<LedgerState>` — the whole
//! ledger's single lock. Every `do_*` method here acquires it exactly once,
//! reads the chain's current state, decides, and either commits every entry
//! the decision produces or commits nothing and returns `Err`. Nothing
//! `.await`s while the guard is live: sink delivery is a synchronous,
//! capacity-checked channel send (never the sink's own `post`, which runs on
//! a background task — see [`LedgerSink`]), so there is no async hook to
//! accidentally call from inside the section.
//!
//! [`Requester`]/[`Approvals`]/[`ApproverHandle`] (`handles.rs`) are thin
//! public wrappers around the `pub(crate)` methods here; this file has no
//! public API of its own.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use kaish_types::approval::{
    ApprovalRequest, ApprovalRequestDraft, AttemptId, AttemptState, Capture, Condition, Expiring,
    Grant, GrantTerms, Grounds, LedgerEntry, Observation, Outcome, Principal, RequestContext,
    RequestId, RequestState, StandingGrant, StandingId, StateClaim, Token,
};
use kaish_types::clock::Instant;

use super::config::{LedgerConfig, LedgerSink};
use super::error::LedgerError;

/// A monotonic clock is `std::time::Instant`/`kaish_types::clock::Instant`
/// everywhere; only the *wall-clock* stamp on each entry is worth
/// substituting in a test, to prove a wall-clock jump cannot move an expiry
/// decision (spec §A.5). Test-only implementations live in this crate's
/// `#[cfg(test)]` modules; production code always uses [`SystemWallClock`].
pub(crate) trait WallClock: Send + Sync {
    fn now(&self) -> SystemTime;
}

#[derive(Debug, Default)]
pub(crate) struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now(&self) -> SystemTime {
        kaish_types::clock::system_now()
    }
}

/// One request's full accumulated state — the ledger's authoritative record.
/// Stays in [`LedgerState::chains`] for the life of the ledger; PR 2
/// deliberately does not evict closed chains from this map (only from the
/// audit ring), so a very long-running process grows it unboundedly. Flagged
/// as a follow-up, not fixed here — see the PR body.
struct Chain {
    request: ApprovalRequest,
    state: RequestState,
    grant: Option<Grant>,
    /// The real credential. `None` until granted; cleared the moment the
    /// chain closes (spec §A.2 — dropped when the chain closes).
    token: Option<Token>,
    reject_count: u32,
    void_reason: Option<String>,
    /// Monotonic — never touched by a wall-clock jump (spec §A.5).
    request_deadline: Instant,
    /// Set at grant time, computed from `not_after` minus the wall-clock
    /// "now" observed at that moment. Also never touched by a later jump.
    grant_deadline: Option<Instant>,
    attempts: HashMap<AttemptId, AttemptRecord>,
    /// At most one attempt may be `Reserved` against a chain at a time
    /// (spec §A.1's "no other attempt against g was still live").
    live_attempt: Option<AttemptId>,
    /// Set when an attempt against this (still nominally `Granted`) chain
    /// settled successfully or `Unknown` — the two outcomes that close a
    /// chain (spec §B.2). `RequestState` has no separate "closed" variant,
    /// so this flag is what `is_closed` reads for the `Granted` case.
    closed_by_settlement: bool,
}

struct AttemptRecord {
    state: AttemptState,
    reserved_at: Instant,
    outcome: Option<Outcome>,
}

impl Chain {
    fn is_closed(&self) -> bool {
        matches!(
            self.state,
            RequestState::Denied | RequestState::Expired | RequestState::Voided | RequestState::Abandoned
        ) || (self.state == RequestState::Granted && self.closed_by_settlement)
    }
}

/// One ring-retained audit entry alongside the request it belongs to (`None`
/// for entries with no single owning request, e.g. `StandingIssued`).
struct RingSlot {
    entry: LedgerEntry,
    request: Option<RequestId>,
}

/// Everything the single mutex protects.
struct LedgerState {
    next_seq: u64,
    next_attempt_seq: u64,
    next_standing_seq: u64,
    chains: HashMap<RequestId, Chain>,
    live_count_total: usize,
    live_count_by_principal: HashMap<String, usize>,
    standing: HashMap<StandingId, StandingGrant>,
    ring: VecDeque<RingSlot>,
    sink_tx: Option<tokio::sync::mpsc::Sender<LedgerEntry>>,
    sink_failed: Arc<AtomicBool>,
}

impl LedgerState {
    fn alloc_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    /// Make room for `n` more entries in the retained ring and confirm the
    /// sink can accept `n` more sends, mutating nothing else. Ring eviction
    /// of already-closed entries is safe to perform even if this call
    /// ultimately returns `Err` for an unrelated reason (sink capacity) —
    /// it never removes information a still-live chain needs.
    fn reserve_capacity(&mut self, n: usize, retained_entries: usize) -> Result<(), LedgerError> {
        while self.ring.len() + n > retained_entries {
            let evictable = self
                .ring
                .front()
                .is_some_and(|slot| match &slot.request {
                    None => true,
                    Some(id) => self.chains.get(id).is_none_or(Chain::is_closed),
                });
            if !evictable {
                return Err(LedgerError::RingAtCapacity);
            }
            self.ring.pop_front();
        }
        if let Some(tx) = &self.sink_tx {
            if self.sink_failed.load(Ordering::Relaxed) {
                return Err(LedgerError::SinkUnavailable(
                    "a prior audit entry failed to record — refusing further privileged operations until the process is restarted".to_string(),
                ));
            }
            if tx.capacity() < n {
                return Err(LedgerError::SinkUnavailable(format!(
                    "audit sink queue is full ({n} more entries needed, 0 available)"
                )));
            }
        }
        Ok(())
    }

    /// Push every entry into the ring and the sink queue. Capacity for all
    /// of them must already have been reserved via [`Self::reserve_capacity`]
    /// under the *same* lock acquisition — no other transaction can have
    /// consumed it in between, so every send here is expected to succeed.
    fn commit(&mut self, entries: Vec<(LedgerEntry, Option<RequestId>)>) -> Vec<LedgerEntry> {
        let mut committed = Vec::with_capacity(entries.len());
        for (entry, request) in entries {
            if let Some(tx) = &self.sink_tx {
                if let Err(err) = tx.try_send(entry.clone()) {
                    // Capacity was just reserved under this same lock, so
                    // this should not happen; if it somehow does (a logic
                    // bug), the entry still lands in the in-memory ring and
                    // state below — only the export is missing this once.
                    tracing::error!(error = %err, "approval ledger: sink send failed despite reserved capacity");
                }
            }
            self.ring.push_back(RingSlot {
                entry: entry.clone(),
                request,
            });
            committed.push(entry);
        }
        committed
    }

    /// Maintain the live counters and drop the credential for a chain the
    /// caller has just transitioned into a closed state (spec §A.2 — the
    /// credential is dropped when the chain closes). Callers set
    /// `state`/`closed_by_settlement` themselves *before* calling this; it
    /// only ever runs once per chain because `LedgerInner`'s transaction
    /// methods each check `is_closed`/`AlreadyDecided`/`Terminal` before
    /// reaching a closing mutation, so the counters never go negative in
    /// practice — `saturating_sub` is defense in depth, not the mechanism.
    fn mark_closed(&mut self, id: &RequestId) {
        self.live_count_total = self.live_count_total.saturating_sub(1);
        if let Some(chain) = self.chains.get_mut(id) {
            let principal = chain.request.principal.id.clone();
            if let Some(count) = self.live_count_by_principal.get_mut(&principal) {
                *count = count.saturating_sub(1);
            }
            chain.token = None;
        }
    }
}

/// The whole ledger's shared, lockable core. Never public — `Requester`,
/// `Approvals`, and `ApproverHandle` (`handles.rs`) are the public surface.
pub(crate) struct LedgerInner {
    /// 32-bit epoch minted once at construction (CSPRNG, not wall-clock —
    /// see `RequestId`'s doc comment for why the id format needs one), so
    /// ids from two ledger instances in the same process never collide.
    epoch: u32,
    config: LedgerConfig,
    state: Mutex<LedgerState>,
    wall: Arc<dyn WallClock>,
}

impl LedgerInner {
    #[allow(clippy::expect_used)] // mirrors nonce.rs's own poisoned-mutex stance
    fn lock(&self) -> std::sync::MutexGuard<'_, LedgerState> {
        self.state.lock().expect("approval ledger mutex poisoned")
    }

    fn now(&self) -> (Instant, SystemTime) {
        (Instant::now(), self.wall.now())
    }

    /// Materialize an `Expired` entry the first time it is observed — on any
    /// read of the request's state, or from the recovery sweep (spec §B.5).
    /// Best-effort is deliberately NOT offered here: like every other
    /// obligation/derived-entry path, a capacity failure here propagates,
    /// because "time passed" deserves the same fail-loud treatment as any
    /// other transaction (see `handles.rs`'s read-side callers for where the
    /// exception to this is — the synchronous `Approvals` read methods,
    /// which cannot return `Result` and so treat this call as best-effort).
    ///
    /// Unlike every public transaction method, this one calls
    /// `emit_events` *before* its caller drops the lock (the caller already
    /// holds `guard` when it calls in, and handing entries back out just to
    /// re-emit them after would touch every one of `materialize_expiry`'s
    /// several call sites for a synchronous `tracing::` call). `tracing`
    /// macros do not `.await`, so this does not violate §B.1's "never call
    /// an async hook while holding the lock" — but a subscriber that blocks
    /// (e.g. writes to a slow file) would hold up every other transaction
    /// on this ledger for the span of that call. Acceptable for now; worth
    /// revisiting if a real subscriber ever makes that latency visible.
    fn materialize_expiry(&self, guard: &mut LedgerState, id: &RequestId, mono: Instant, wall: SystemTime) -> Result<(), LedgerError> {
        let Some(chain) = guard.chains.get(id) else {
            return Ok(());
        };
        let what = match chain.state {
            RequestState::Requested if mono >= chain.request_deadline => {
                Some(Expiring::Request)
            }
            RequestState::Granted if !chain.closed_by_settlement => match chain.grant_deadline {
                Some(deadline) if mono >= deadline => Some(Expiring::Grant),
                _ => None,
            },
            _ => None,
        };
        let Some(what) = what else {
            return Ok(());
        };
        guard.reserve_capacity(1, self.config.retained_entries)?;
        let seq = guard.alloc_seq();
        let entries = vec![(
            LedgerEntry::Expired {
                seq,
                at: wall,
                request: id.clone(),
                what,
            },
            Some(id.clone()),
        )];
        let committed = guard.commit(entries);
        if let Some(chain) = guard.chains.get_mut(id) {
            chain.state = RequestState::Expired;
        }
        guard.mark_closed(id);
        emit_events(&committed);
        Ok(())
    }

    // ── Obligations (Requester) ──────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn post_request(
        &self,
        draft: ApprovalRequestDraft,
        principal: Principal,
        capture: Capture,
        context: RequestContext,
        ttl: Duration,
        job_id: Option<u64>,
    ) -> Result<ApprovalRequest, LedgerError> {
        let (mono, wall) = self.now();
        let mut guard = self.lock();

        if guard.live_count_total >= self.config.live_capacity {
            return Err(LedgerError::LiveCapacity {
                limit: self.config.live_capacity,
            });
        }
        let per_principal = *guard.live_count_by_principal.get(&principal.id).unwrap_or(&0);
        if per_principal >= self.config.live_capacity_per_principal {
            return Err(LedgerError::LiveCapacityPerPrincipal {
                principal: principal.id.clone(),
                limit: self.config.live_capacity_per_principal,
            });
        }
        guard.reserve_capacity(1, self.config.retained_entries)?;

        let seq = guard.alloc_seq();
        let id = RequestId::new(self.epoch, seq);
        let request = draft.stamp(id.clone(), principal.clone(), capture, context, wall, ttl, job_id);
        let chain = Chain {
            request: request.clone(),
            state: RequestState::Requested,
            grant: None,
            token: None,
            reject_count: 0,
            void_reason: None,
            request_deadline: mono + ttl,
            grant_deadline: None,
            attempts: HashMap::new(),
            live_attempt: None,
            closed_by_settlement: false,
        };
        guard.chains.insert(id.clone(), chain);
        guard.live_count_total += 1;
        *guard.live_count_by_principal.entry(principal.id).or_insert(0) += 1;

        let entries = vec![(
            LedgerEntry::Requested {
                seq,
                at: wall,
                request: request.clone(),
            },
            Some(id),
        )];
        let committed = guard.commit(entries);
        drop(guard);
        emit_events(&committed);
        Ok(request)
    }

    pub(crate) fn redeem(
        &self,
        id: &RequestId,
        by: Principal,
        observed: Vec<Observation>,
    ) -> Result<AttemptId, LedgerError> {
        let (mono, wall) = self.now();
        let mut guard = self.lock();
        self.materialize_expiry(&mut guard, id, mono, wall)?;
        let (result, committed) = self.redeem_locked(&mut guard, id, by, observed, mono, wall);
        drop(guard);
        emit_events(&committed);
        result
    }

    pub(crate) fn redeem_with_token(
        &self,
        id: &RequestId,
        presented: &str,
        by: Principal,
        observed: Vec<Observation>,
    ) -> Result<AttemptId, LedgerError> {
        let (mono, wall) = self.now();
        let mut guard = self.lock();
        self.materialize_expiry(&mut guard, id, mono, wall)?;

        let Some(chain) = guard.chains.get(id) else {
            // A guessed id that matches nothing counts against nothing, so a
            // guesser cannot void a request it cannot describe (spec §F.3).
            // Best-effort: if the ring/sink has no room even for this one
            // bookkeeping entry, skip recording it rather than failing a
            // rejection that was never going to succeed anyway — seq is
            // only allocated once capacity is confirmed, so a skip here
            // never opens a gap.
            if guard.reserve_capacity(1, self.config.retained_entries).is_ok() {
                let seq = guard.alloc_seq();
                let entries = vec![(
                    LedgerEntry::TokenRejected {
                        seq,
                        at: wall,
                        request: None,
                        attempts: 0,
                    },
                    None,
                )];
                let committed = guard.commit(entries);
                drop(guard);
                emit_events(&committed);
            }
            return Err(LedgerError::NotAuthorized(id.clone()));
        };

        // A request that already closed (Denied/Expired/Voided/Abandoned, or
        // Granted-and-settled) is a *known* request, not an absent one — any
        // presentation against it (right or wrong; its credential is gone,
        // cleared at close) fails naming what happened, with no further
        // `TokenRejected` bookkeeping against an already-dead chain (spec
        // §F.3: "a later good key fails naming the void").
        if chain.is_closed() {
            let err = if chain.state == RequestState::Granted && chain.closed_by_settlement {
                let outcome = chain
                    .attempts
                    .values()
                    .find_map(|a| if matches!(a.state, AttemptState::Settled) { a.outcome.clone() } else { None });
                LedgerError::AlreadySettled {
                    id: id.clone(),
                    outcome,
                }
            } else {
                self.terminal_error(id, chain.state, chain.void_reason.clone())
            };
            return Err(err);
        }

        let matches_real_token = guard
            .chains
            .get(id)
            .and_then(|c| c.token.as_ref())
            .is_some_and(|t| t.reveal() == presented);

        if matches_real_token {
            let (result, committed) = self.redeem_locked(&mut guard, id, by, observed, mono, wall);
            drop(guard);
            emit_events(&committed);
            return result;
        }

        // A live, known request, but the presented credential is wrong (or,
        // for a still-`Requested` chain, no real credential exists yet to
        // match at all — every presentation against it is "bad" by
        // definition, and still counts, matching the transition table's
        // "redeem before any decision" row).
        //
        // Compute the *would-be* count without mutating anything yet — a
        // capacity failure below must leave `reject_count` untouched
        // (commit-or-nothing, spec §B.1), or a later successful rejection
        // would record an `attempts` value one higher than the number of
        // `TokenRejected` entries actually on the log.
        let Some(chain) = guard.chains.get(id) else {
            return Err(LedgerError::NotAuthorized(id.clone()));
        };
        let n = chain.reject_count + 1;
        let voids_now = n >= self.config.max_token_attempts;
        guard.reserve_capacity(if voids_now { 2 } else { 1 }, self.config.retained_entries)?;
        if let Some(chain) = guard.chains.get_mut(id) {
            chain.reject_count = n;
        }

        let seq1 = guard.alloc_seq();
        let mut entries = vec![(
            LedgerEntry::TokenRejected {
                seq: seq1,
                at: wall,
                request: Some(id.clone()),
                attempts: n,
            },
            Some(id.clone()),
        )];
        if voids_now {
            let reason = format!("voided after {n} invalid credential attempts");
            if let Some(chain) = guard.chains.get_mut(id) {
                chain.state = RequestState::Voided;
                chain.void_reason = Some(reason.clone());
            }
            guard.mark_closed(id);
            let seq2 = guard.alloc_seq();
            entries.push((
                LedgerEntry::Voided {
                    seq: seq2,
                    at: wall,
                    request: id.clone(),
                    reason,
                },
                Some(id.clone()),
            ));
        }
        let committed = guard.commit(entries);
        drop(guard);
        emit_events(&committed);
        Err(LedgerError::NotAuthorized(id.clone()))
    }

    /// The shared core of both redemption entry points: state check,
    /// already-settled check, in-flight check, condition evaluation,
    /// reservation. Callers have already materialized expiry and verified
    /// (or bypassed, for the internal-context path) the credential.
    ///
    /// Always returns the entries this call committed alongside the
    /// outcome — the `Refused`+`Voided` path commits two entries and still
    /// returns `Err`, so the caller cannot just `?` this and must emit
    /// events from the returned vec regardless of which arm fired.
    fn redeem_locked(
        &self,
        guard: &mut LedgerState,
        id: &RequestId,
        by: Principal,
        observed: Vec<Observation>,
        mono: Instant,
        wall: SystemTime,
    ) -> (Result<AttemptId, LedgerError>, Vec<LedgerEntry>) {
        let Some(chain) = guard.chains.get(id) else {
            return (Err(LedgerError::NotFound(id.clone())), Vec::new());
        };
        match chain.state {
            RequestState::Requested => return (Err(LedgerError::NotAuthorized(id.clone())), Vec::new()),
            RequestState::Granted => {}
            other => {
                let err = self.terminal_error(id, other, chain.void_reason.clone());
                return (Err(err), Vec::new());
            }
        }
        if chain.closed_by_settlement {
            let outcome = chain
                .attempts
                .values()
                .find_map(|a| if matches!(a.state, AttemptState::Settled) { a.outcome.clone() } else { None });
            return (
                Err(LedgerError::AlreadySettled {
                    id: id.clone(),
                    outcome,
                }),
                Vec::new(),
            );
        }
        if chain.live_attempt.is_some() {
            return (Err(LedgerError::AttemptInFlight(id.clone())), Vec::new());
        }
        let Some(grant) = chain.grant.clone() else {
            debug_assert!(false, "chain state is Granted but no Grant is stored");
            let err = LedgerError::InvariantViolated(format!("request {id} is Granted but has no stored Grant"));
            return (Err(err), Vec::new());
        };

        let mut refusal: Option<(Condition, StateClaim)> = None;
        for condition in &grant.conditions {
            let claim = observed
                .iter()
                .find(|o| o.resource == condition.resource)
                .map(|o| o.claim.clone());
            let holds = matches!(&claim, Some(c) if *c == condition.expected_from);
            if !holds {
                refusal = Some((condition.clone(), claim.unwrap_or(StateClaim::Unspecified)));
                break;
            }
        }

        if let Some((condition, found)) = refusal {
            if let Err(err) = guard.reserve_capacity(2, self.config.retained_entries) {
                return (Err(err), Vec::new());
            }
            let seq1 = guard.alloc_seq();
            let reason = "redemption-time conditions no longer hold".to_string();
            let mut entries = vec![(
                LedgerEntry::Refused {
                    seq: seq1,
                    at: wall,
                    request: id.clone(),
                    condition,
                    found,
                },
                Some(id.clone()),
            )];
            if let Some(chain) = guard.chains.get_mut(id) {
                chain.state = RequestState::Voided;
                chain.void_reason = Some(reason.clone());
            }
            guard.mark_closed(id);
            let seq2 = guard.alloc_seq();
            entries.push((
                LedgerEntry::Voided {
                    seq: seq2,
                    at: wall,
                    request: id.clone(),
                    reason: reason.clone(),
                },
                Some(id.clone()),
            ));
            let committed = guard.commit(entries);
            return (
                Err(LedgerError::Refused {
                    id: id.clone(),
                    detail: reason,
                }),
                committed,
            );
        }

        if let Err(err) = guard.reserve_capacity(1, self.config.retained_entries) {
            return (Err(err), Vec::new());
        }
        let attempt_seq = guard.next_attempt_seq;
        guard.next_attempt_seq += 1;
        let attempt_id = AttemptId::new(attempt_seq);
        let seq = guard.alloc_seq();
        let entry = LedgerEntry::Redeemed {
            seq,
            at: wall,
            request: id.clone(),
            attempt: attempt_id,
            by,
            observed,
        };
        if let Some(chain) = guard.chains.get_mut(id) {
            chain.attempts.insert(
                attempt_id,
                AttemptRecord {
                    state: AttemptState::Reserved,
                    reserved_at: mono,
                    outcome: None,
                },
            );
            chain.live_attempt = Some(attempt_id);
        }
        let committed = guard.commit(vec![(entry, Some(id.clone()))]);
        (Ok(attempt_id), committed)
    }

    pub(crate) fn settle(
        &self,
        request_id: &RequestId,
        attempt_id: AttemptId,
        outcome: Outcome,
    ) -> Result<bool, LedgerError> {
        let (_, wall) = self.now();
        let mut guard = self.lock();
        let Some(chain) = guard.chains.get(request_id) else {
            return Err(LedgerError::NotFound(request_id.clone()));
        };
        let Some(record) = chain.attempts.get(&attempt_id) else {
            debug_assert!(false, "settle() named an AttemptId never reserved against this request");
            return Err(LedgerError::InvariantViolated(format!(
                "settle: attempt {attempt_id} was never reserved against request {request_id}"
            )));
        };
        if !matches!(record.state, AttemptState::Reserved) {
            // Idempotent by AttemptId (spec §A.1): the first settlement won.
            return Ok(false);
        }
        let closes = matches!(
            outcome,
            Outcome::Exit(0) | Outcome::Unknown { .. }
        );
        if closes && chain.closed_by_settlement {
            debug_assert!(false, "a second successful settlement was attempted against one grant");
            return Err(LedgerError::InvariantViolated(format!(
                "request {request_id} already has a successful (or Unknown) settlement — a grant authorizes exactly one"
            )));
        }
        // The chain may already have closed a different way (voided by a
        // 5th bad credential, expired past `not_after`, abandoned) while
        // this attempt was still `Reserved` — that path does not check
        // `live_attempt`, by design (spec §B.2: those are derived facts
        // about the world, not about any one attempt). A chain closes
        // exactly once; `mark_closed` must not run a second time here, or
        // the live counters it maintains undercount (spec §D.4's
        // `live_capacity` gate would then admit more than its configured
        // number of genuinely live requests).
        let was_already_closed = chain.is_closed();

        guard.reserve_capacity(1, self.config.retained_entries)?;
        let seq = guard.alloc_seq();
        let entries = vec![(
            LedgerEntry::Settled {
                seq,
                at: wall,
                request: request_id.clone(),
                attempt: attempt_id,
                outcome: outcome.clone(),
            },
            Some(request_id.clone()),
        )];
        let committed = guard.commit(entries);
        if let Some(chain) = guard.chains.get_mut(request_id) {
            if let Some(record) = chain.attempts.get_mut(&attempt_id) {
                record.state = AttemptState::Settled;
                record.outcome = Some(outcome);
            }
            chain.live_attempt = None;
            if closes {
                chain.closed_by_settlement = true;
            }
        }
        if closes && !was_already_closed {
            guard.mark_closed(request_id);
        }
        drop(guard);
        emit_events(&committed);
        Ok(true)
    }

    pub(crate) fn abandon_request(&self, id: &RequestId, reason: String) -> Result<(), LedgerError> {
        let (mono, wall) = self.now();
        let mut guard = self.lock();
        self.materialize_expiry(&mut guard, id, mono, wall)?;
        let Some(chain) = guard.chains.get(id) else {
            return Err(LedgerError::NotFound(id.clone()));
        };
        if chain.is_closed() {
            return Err(self.terminal_error(id, chain.state, chain.void_reason.clone()));
        }
        if chain.live_attempt.is_some() {
            return Err(LedgerError::AttemptInFlight(id.clone()));
        }
        guard.reserve_capacity(1, self.config.retained_entries)?;
        let seq = guard.alloc_seq();
        let entries = vec![(
            LedgerEntry::Abandoned {
                seq,
                at: wall,
                request: id.clone(),
                attempt: None,
                reason,
            },
            Some(id.clone()),
        )];
        let committed = guard.commit(entries);
        if let Some(chain) = guard.chains.get_mut(id) {
            chain.state = RequestState::Abandoned;
        }
        guard.mark_closed(id);
        drop(guard);
        emit_events(&committed);
        Ok(())
    }

    /// Renew an `Expired` request into a fresh `Requested` one (spec §B.5).
    ///
    /// Deliberately **not** one critical section, unlike every other method
    /// in this file — it acquires the lock up to three times (materialize,
    /// read, then `post_request`'s own). This is safe only because the
    /// state it reads back (`Expired`) is terminal for every path except
    /// this one: nothing else in PR 2 can mutate an already-`Expired`
    /// chain out from under the second acquisition. It stays flagged here
    /// rather than fixed because the real fix — reading the old request and
    /// posting the new one under one held guard — needs `post_request`'s
    /// internals threaded through a shared-guard variant, which is not
    /// worth doing until `renew` gains its full behavior (re-observing the
    /// transitions before posting; see the note below).
    ///
    /// **Does not yet re-observe transitions before posting** (the rest of
    /// spec §B.5: "If the world already moved, renewal fails loud rather
    /// than posting a request whose claims are already false"). The
    /// original resources — which may carry `StateClaim::Exact` transition
    /// claims — are cloned verbatim. Re-observation needs a `StateResolver`
    /// call per resource, which is PR 6's `StateResolver` trait; PR 2 has
    /// no I/O seam to call one through. A `renew`d request's conditions are
    /// still re-checked at its own redemption time (the ordinary
    /// `Refused`/`Voided` path), so a stale claim is caught there — just one
    /// step later than the spec's stated ideal.
    pub(crate) fn renew(&self, id: &RequestId) -> Result<RequestId, LedgerError> {
        let (mono, wall) = self.now();
        {
            let mut guard = self.lock();
            self.materialize_expiry(&mut guard, id, mono, wall)?;
        }
        let guard = self.lock();
        let Some(chain) = guard.chains.get(id) else {
            return Err(LedgerError::NotFound(id.clone()));
        };
        if chain.state != RequestState::Expired {
            return Err(LedgerError::NotRenewable {
                id: id.clone(),
                state: chain.state,
            });
        }
        let old = chain.request.clone();
        drop(guard);

        // `ApprovalRequestDraft` is `#[non_exhaustive]` with no external
        // constructor besides the builder — rebuild through it rather than
        // a struct literal. `operation`/`risk` were valid on the original
        // request, so this should never fail; treat it as a bug if it does.
        let mut builder = kaish_types::approval::ApprovalRequest::builder(old.operation.as_str().to_string())
            .risk(old.risk)
            .reason(old.reason)
            .hint(old.hint)
            .supersedes(id.clone());
        for resource in old.resources {
            builder = builder.resource(resource);
        }
        let draft = builder.build().map_err(|e| {
            debug_assert!(false, "renew: rebuilding a draft from an already-valid request failed: {e}");
            LedgerError::InvariantViolated(format!("renew: failed to rebuild request {id} as a draft: {e}"))
        })?;
        self.post_request(draft, old.principal, old.capture, old.context, old.ttl, old.job_id)
            .map(|req| req.id)
    }

    fn terminal_error(&self, id: &RequestId, state: RequestState, void_reason: Option<String>) -> LedgerError {
        match state {
            RequestState::Granted => LedgerError::AlreadyDecided(id.clone()),
            _ => LedgerError::Terminal {
                id: id.clone(),
                state,
                detail: void_reason,
            },
        }
    }

    // ── Authorizations (ApproverHandle) ─────────────────────────────

    pub(crate) fn grant(
        &self,
        id: &RequestId,
        terms: GrantTerms,
        decided_by: Principal,
        grounds: Grounds,
    ) -> Result<Grant, LedgerError> {
        // Drawn before the lock is taken: `getrandom::fill` is synchronous
        // but can block on entropy starvation, and the ledger lock must
        // never gate on I/O — even non-async I/O — the way it never gates
        // on `Approver::decide` (spec §B.1, §C.2). A grant that turns out
        // to be invalid (already decided, terminal) has drawn entropy for
        // nothing; that is a cheap, inconsequential cost next to the
        // alternative of blocking every other transaction on this ledger.
        let token = Token::new(
            generate_credential().map_err(|e| LedgerError::CredentialUnavailable(e.to_string()))?,
        );
        let (mono, wall) = self.now();
        let mut guard = self.lock();
        self.materialize_expiry(&mut guard, id, mono, wall)?;
        let Some(chain) = guard.chains.get(id) else {
            return Err(LedgerError::NotFound(id.clone()));
        };
        match chain.state {
            RequestState::Requested => {}
            RequestState::Granted => return Err(LedgerError::AlreadyDecided(id.clone())),
            other => return Err(self.terminal_error(id, other, chain.void_reason.clone())),
        }
        guard.reserve_capacity(1, self.config.retained_entries)?;

        let not_after = terms.not_after;
        let grant = Grant::from_terms(id.clone(), decided_by, grounds, terms, token.token_prefix(), wall);
        let remaining = not_after.duration_since(wall).unwrap_or(Duration::ZERO);

        let seq = guard.alloc_seq();
        let entries = vec![(
            LedgerEntry::Granted {
                seq,
                at: wall,
                grant: grant.clone(),
            },
            Some(id.clone()),
        )];
        let committed = guard.commit(entries);
        if let Some(chain) = guard.chains.get_mut(id) {
            chain.grant = Some(grant.clone());
            chain.state = RequestState::Granted;
            chain.grant_deadline = Some(mono + remaining);
            chain.token = Some(token);
        }
        drop(guard);
        emit_events(&committed);
        Ok(grant)
    }

    pub(crate) fn deny(&self, id: &RequestId, reason: String, by: Principal) -> Result<(), LedgerError> {
        let (mono, wall) = self.now();
        let mut guard = self.lock();
        self.materialize_expiry(&mut guard, id, mono, wall)?;
        let Some(chain) = guard.chains.get(id) else {
            return Err(LedgerError::NotFound(id.clone()));
        };
        match chain.state {
            RequestState::Requested => {}
            RequestState::Granted => return Err(LedgerError::AlreadyDecided(id.clone())),
            other => return Err(self.terminal_error(id, other, chain.void_reason.clone())),
        }
        guard.reserve_capacity(1, self.config.retained_entries)?;
        let seq = guard.alloc_seq();
        let entries = vec![(
            LedgerEntry::Denied {
                seq,
                at: wall,
                request: id.clone(),
                by,
                reason,
            },
            Some(id.clone()),
        )];
        let committed = guard.commit(entries);
        if let Some(chain) = guard.chains.get_mut(id) {
            chain.state = RequestState::Denied;
        }
        guard.mark_closed(id);
        drop(guard);
        emit_events(&committed);
        Ok(())
    }

    pub(crate) fn grant_standing(&self, mut standing: StandingGrant) -> Result<StandingId, LedgerError> {
        let (_, wall) = self.now();
        let mut guard = self.lock();
        guard.reserve_capacity(1, self.config.retained_entries)?;
        let raw = guard.next_standing_seq;
        guard.next_standing_seq += 1;
        let id = StandingId::new(raw);
        standing.id = id;
        guard.standing.insert(id, standing.clone());
        let seq = guard.alloc_seq();
        let entries = vec![(
            LedgerEntry::StandingIssued {
                seq,
                at: wall,
                grant: standing,
            },
            None,
        )];
        let committed = guard.commit(entries);
        drop(guard);
        emit_events(&committed);
        Ok(id)
    }

    pub(crate) fn revoke_standing(&self, id: StandingId, by: Principal, reason: String) -> Result<(), LedgerError> {
        let (_, wall) = self.now();
        let mut guard = self.lock();
        if !guard.standing.contains_key(&id) {
            return Err(LedgerError::StandingNotFound(id));
        }
        guard.reserve_capacity(1, self.config.retained_entries)?;
        guard.standing.remove(&id);
        let seq = guard.alloc_seq();
        let entries = vec![(
            LedgerEntry::StandingRevoked {
                seq,
                at: wall,
                id,
                by,
                reason,
            },
            None,
        )];
        let committed = guard.commit(entries);
        drop(guard);
        emit_events(&committed);
        Ok(())
    }

    /// Retrieve the credential. Appends `KeyRetrieved` naming `by`. Returns
    /// `None` both when the request has no live credential yet/anymore AND
    /// (best-effort) when the ring/sink has no room to record the
    /// retrieval — this method's `Option`-only signature (spec §D.2) has no
    /// way to distinguish those from the caller's side.
    pub(crate) fn token_for(&self, id: &RequestId, by: Principal) -> Option<Token> {
        let (_, wall) = self.now();
        let mut guard = self.lock();
        let token = guard.chains.get(id)?.token.clone()?;
        if guard.reserve_capacity(1, self.config.retained_entries).is_err() {
            return Some(token);
        }
        let seq = guard.alloc_seq();
        let entries = vec![(
            LedgerEntry::KeyRetrieved {
                seq,
                at: wall,
                request: id.clone(),
                by,
            },
            Some(id.clone()),
        )];
        let committed = guard.commit(entries);
        drop(guard);
        emit_events(&committed);
        Some(token)
    }

    // ── Recovery sweep ───────────────────────────────────────────────

    /// Materializes due `Expired` entries and closes `Reserved` attempts
    /// that have sat unreported past `LedgerConfig::attempt_stale_after` as
    /// `Abandoned` (spec §D.4's recovery sweep — see `attempt_stale_after`'s
    /// doc comment for why PR 2 needs a staleness bound at all).
    pub(crate) fn sweep(&self) {
        let (mono, wall) = self.now();
        let ids: Vec<RequestId> = {
            let guard = self.lock();
            guard.chains.keys().cloned().collect()
        };
        for id in ids {
            let mut guard = self.lock();
            let _ = self.materialize_expiry(&mut guard, &id, mono, wall);
            drop(guard);
            self.sweep_stale_attempt(&id, mono, wall);
        }
    }

    fn sweep_stale_attempt(&self, id: &RequestId, mono: Instant, wall: SystemTime) {
        let mut guard = self.lock();
        let Some(chain) = guard.chains.get(id) else { return };
        let Some(attempt_id) = chain.live_attempt else { return };
        let Some(record) = chain.attempts.get(&attempt_id) else { return };
        if !matches!(record.state, AttemptState::Reserved) {
            return;
        }
        if mono.duration_since(record.reserved_at) < self.config.attempt_stale_after {
            return;
        }
        if guard.reserve_capacity(1, self.config.retained_entries).is_err() {
            return;
        }
        let seq = guard.alloc_seq();
        let reason = "recovery sweep: reservation exceeded the staleness bound with no report".to_string();
        let entries = vec![(
            LedgerEntry::Abandoned {
                seq,
                at: wall,
                request: id.clone(),
                attempt: Some(attempt_id),
                reason,
            },
            Some(id.clone()),
        )];
        let committed = guard.commit(entries);
        if let Some(chain) = guard.chains.get_mut(id) {
            if let Some(record) = chain.attempts.get_mut(&attempt_id) {
                record.state = AttemptState::Abandoned;
            }
            chain.live_attempt = None;
            // An abandoned attempt's effects are unknown, same as
            // `Outcome::Unknown` — the chain closes rather than staying
            // open for a retry against a grant nobody can vouch for.
            chain.closed_by_settlement = true;
        }
        guard.mark_closed(id);
        drop(guard);
        emit_events(&committed);
    }

    // ── Read side (Approvals) ────────────────────────────────────────

    pub(crate) fn pending(&self) -> Vec<ApprovalRequest> {
        // Reuses the full sweep (expiry materialization across every chain)
        // rather than a narrower per-id check — `pending()` doesn't know in
        // advance which ids are due, and the sweep's own capacity failures
        // are already best-effort (swallowed), matching this method's
        // `Result`-free signature.
        self.sweep();
        let guard = self.lock();
        guard
            .chains
            .values()
            .filter(|c| c.state == RequestState::Requested)
            .map(|c| c.request.clone())
            .collect()
    }

    pub(crate) fn state(&self, id: &RequestId) -> Option<RequestState> {
        self.best_effort_materialize(id);
        let guard = self.lock();
        guard.chains.get(id).map(|c| c.state)
    }

    pub(crate) fn chain(&self, id: &RequestId) -> Option<super::handles::RequestChain> {
        self.best_effort_materialize(id);
        let guard = self.lock();
        let chain = guard.chains.get(id)?;
        Some(super::handles::RequestChain {
            request: (&chain.request).into(),
            state: chain.state,
            grant: chain.grant.clone(),
            attempts: chain
                .attempts
                .iter()
                .map(|(id, record)| super::handles::AttemptView {
                    attempt: *id,
                    state: record.state,
                    outcome: record.outcome.clone(),
                })
                .collect(),
        })
    }

    pub(crate) fn standing(&self) -> Vec<StandingGrant> {
        let guard = self.lock();
        guard.standing.values().cloned().collect()
    }

    pub(crate) fn log(&self, since: u64) -> Vec<LedgerEntry> {
        let guard = self.lock();
        guard
            .ring
            .iter()
            .map(|slot| &slot.entry)
            .filter(|entry| entry.seq() > since)
            .cloned()
            .collect()
    }

    /// `Approvals`' read methods return no `Result` (spec §D.2), so unlike
    /// every write-side path, a capacity failure while materializing due
    /// expiry here is swallowed — the read still returns the (briefly)
    /// stale state rather than panicking or blocking. See
    /// `materialize_expiry`'s doc comment for the write-side contrast.
    fn best_effort_materialize(&self, id: &RequestId) {
        let (mono, wall) = self.now();
        let mut guard = self.lock();
        let _ = self.materialize_expiry(&mut guard, id, mono, wall);
    }

}

/// One event per appended entry, at the same call site the entry itself was
/// committed at — "no second place where a ledger fact can be recorded
/// without a trace fact" (spec §G). Levels match the spec's Events table.
fn emit_events(entries: &[LedgerEntry]) {
    for entry in entries {
        match entry {
            LedgerEntry::Requested { request, .. } => {
                tracing::info!(request_id = %request.id, operation = %request.operation, "approval.requested");
            }
            LedgerEntry::Granted { grant, .. } => {
                tracing::info!(request_id = %grant.request, "approval.granted");
            }
            LedgerEntry::Denied { request, .. } => {
                tracing::info!(request_id = %request, "approval.denied");
            }
            LedgerEntry::Expired { request, .. } => {
                tracing::info!(request_id = %request, "approval.expired");
            }
            LedgerEntry::KeyRetrieved { request, by, .. } => {
                tracing::info!(request_id = %request, retrieved_by = %by.id, "approval.key_retrieved");
            }
            LedgerEntry::Redeemed { request, attempt, .. } => {
                tracing::debug!(request_id = %request, attempt_id = %attempt, "approval.redeemed");
            }
            LedgerEntry::Refused { request, .. } => {
                tracing::warn!(request_id = %request, "approval.refused");
            }
            LedgerEntry::Settled { request, attempt, .. } => {
                tracing::info!(request_id = %request, attempt_id = %attempt, "approval.settled");
            }
            LedgerEntry::Abandoned { request, .. } => {
                tracing::warn!(request_id = %request, "approval.abandoned");
            }
            LedgerEntry::Voided { request, .. } => {
                tracing::warn!(request_id = %request, "approval.voided");
            }
            LedgerEntry::StandingIssued { grant, .. } => {
                tracing::info!(standing_id = %grant.id, "approval.standing_issued");
            }
            LedgerEntry::StandingRevoked { id, .. } => {
                tracing::info!(standing_id = %id, "approval.standing_revoked");
            }
            LedgerEntry::TokenRejected { request, attempts, .. } => {
                tracing::warn!(request_id = ?request.as_ref().map(ToString::to_string), attempts = attempts, "approval.token_rejected");
            }
            // `LedgerEntry` is `#[non_exhaustive]` from this crate's side,
            // so this match needs a wildcard even though every variant that
            // exists today is covered above (kaish-types' own `impl
            // LedgerEntry` — see `seq()` — is where a genuinely exhaustive
            // match against a new variant belongs; this one just loses its
            // event, loudly, in debug).
            other => {
                debug_assert!(false, "approval ledger: no tracing event wired for entry variant: {other:?}");
            }
        }
    }
}

/// 128 bits from `getrandom`, 32 lowercase hex — identical construction to
/// `nonce.rs`'s `generate_nonce` (kaish #259), duplicated rather than shared
/// because `nonce.rs` is deleted outright in the cutover (PR 5) and this
/// type should not depend on code scheduled for removal.
fn generate_credential() -> Result<String, getrandom::Error> {
    let mut entropy = [0u8; 16];
    getrandom::fill(&mut entropy)?;
    Ok(entropy.iter().map(|b| format!("{b:02x}")).collect())
}

/// Mint a ledger epoch: 32 bits from `getrandom`, so `RequestId`s from two
/// ledger instances in the same process never collide (spec §A.2's id
/// format needs an epoch; nothing says it must be predictable).
pub(crate) fn generate_epoch() -> Result<u32, getrandom::Error> {
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

pub(crate) fn build_inner(
    config: LedgerConfig,
    sink: Option<Arc<dyn LedgerSink>>,
    wall: Arc<dyn WallClock>,
) -> Result<Arc<LedgerInner>, getrandom::Error> {
    let epoch = generate_epoch()?;
    let sink_failed = Arc::new(AtomicBool::new(false));
    let sink_tx = sink.as_ref().map(|sink| {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<LedgerEntry>(config.sink_queue.max(1));
        let sink = Arc::clone(sink);
        let failed = Arc::clone(&sink_failed);
        tokio::spawn(async move {
            while let Some(entry) = rx.recv().await {
                if let Err(err) = sink.post(&entry) {
                    tracing::error!(error = %err, "approval ledger: audit sink failed — refusing further obligations");
                    failed.store(true, Ordering::Relaxed);
                    break;
                }
            }
        });
        tx
    });

    let state = LedgerState {
        next_seq: 1,
        next_attempt_seq: 1,
        next_standing_seq: 1,
        chains: HashMap::new(),
        live_count_total: 0,
        live_count_by_principal: HashMap::new(),
        standing: HashMap::new(),
        ring: VecDeque::new(),
        sink_tx,
        sink_failed,
    };

    Ok(Arc::new(LedgerInner {
        epoch,
        config,
        state: Mutex::new(state),
        wall,
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI64;

    use kaish_types::approval::{ApprovalRequest, AttemptId, PrincipalKind, RiskClass};

    use super::*;

    /// A wall clock a test can jump forward or backward independently of
    /// the real monotonic clock, to prove expiry math never reads it (spec
    /// §A.5). There is no way to fake `kaish_types::clock::Instant` itself
    /// (no public constructor) — nor is one needed, since the property
    /// under test is precisely that expiry decisions never consult this
    /// clock at all.
    struct FakeWallClock {
        offset_secs: AtomicI64,
    }

    impl WallClock for FakeWallClock {
        fn now(&self) -> SystemTime {
            let base = kaish_types::clock::system_now();
            let offset = self.offset_secs.load(Ordering::Relaxed);
            if offset >= 0 {
                base + Duration::from_secs(offset as u64)
            } else {
                base - Duration::from_secs((-offset) as u64)
            }
        }
    }

    fn agent(id: &str) -> Principal {
        Principal::new(id, PrincipalKind::Agent)
    }

    #[allow(clippy::unwrap_used)]
    fn draft(op: &str) -> ApprovalRequestDraft {
        ApprovalRequest::builder(op).risk(RiskClass::Reversible).build().unwrap()
    }

    /// Regression test for a review finding: a bad-credential presentation
    /// must reserve ring/sink capacity for its `TokenRejected` entry
    /// *before* touching `reject_count`, or a capacity failure leaves the
    /// counter advanced with no corresponding entry — the next successful
    /// rejection would then report an `attempts` value one higher than the
    /// number of `TokenRejected` entries actually on the log, and the fifth
    /// void could fire after only four recorded rejections.
    #[test]
    fn bad_key_under_ring_pressure_does_not_advance_reject_count_without_recording_it() {
        let config = LedgerConfig {
            retained_entries: 1,
            ..Default::default()
        };
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(config, None, Arc::new(SystemWallClock)).unwrap();
        let principal = agent("agent-1");
        // Occupies the ring's one slot with a still-live (Requested, no
        // decision yet) chain — nothing is evictable, so any further
        // append attempt must refuse loud rather than partially commit.
        #[allow(clippy::unwrap_used)]
        let req = inner
            .post_request(draft("fs.remove"), principal.clone(), Capture::DirectExecution, RequestContext::default(), Duration::from_secs(60), None)
            .unwrap();

        let err = inner
            .redeem_with_token(&req.id, "wrong", principal, Vec::new())
            .unwrap_err();
        assert!(matches!(err, LedgerError::RingAtCapacity), "got {err:?}");

        #[allow(clippy::unwrap_used)]
        let reject_count = {
            let guard = inner.lock();
            guard.chains.get(&req.id).unwrap().reject_count
        };
        assert_eq!(reject_count, 0, "a capacity failure must not silently advance the rejection counter");
    }

    #[test]
    fn wall_clock_jumps_neither_extend_nor_void_a_grant() {
        let clock = Arc::new(FakeWallClock {
            offset_secs: AtomicI64::new(0),
        });
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(LedgerConfig::default(), None, clock.clone()).unwrap();
        let principal = agent("agent-1");
        #[allow(clippy::unwrap_used)]
        let req = inner
            .post_request(draft("fs.remove"), principal.clone(), Capture::DirectExecution, RequestContext::default(), Duration::from_secs(60), None)
            .unwrap();
        let not_after = clock.now() + Duration::from_secs(300);
        #[allow(clippy::unwrap_used)]
        inner
            .grant(&req.id, GrantTerms::once_for(&req, not_after), principal, Grounds::Embedder)
            .unwrap();
        assert_eq!(inner.state(&req.id), Some(RequestState::Granted));

        clock.offset_secs.store(100_000_000, Ordering::Relaxed);
        assert_eq!(
            inner.state(&req.id),
            Some(RequestState::Granted),
            "a forward wall-clock jump must not expire the grant"
        );

        clock.offset_secs.store(-100_000_000, Ordering::Relaxed);
        assert_eq!(
            inner.state(&req.id),
            Some(RequestState::Granted),
            "a backward wall-clock jump must not extend or void the grant either"
        );
    }

    #[test]
    #[should_panic(expected = "second successful settlement")]
    fn second_successful_settlement_against_one_grant_is_invariant_violated() {
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(LedgerConfig::default(), None, Arc::new(SystemWallClock)).unwrap();
        let principal = agent("agent-1");
        #[allow(clippy::unwrap_used)]
        let req = inner
            .post_request(draft("fs.remove"), principal.clone(), Capture::DirectExecution, RequestContext::default(), Duration::from_secs(60), None)
            .unwrap();
        let not_after = SystemTime::now() + Duration::from_secs(300);
        #[allow(clippy::unwrap_used)]
        inner
            .grant(&req.id, GrantTerms::once_for(&req, not_after), principal.clone(), Grounds::Embedder)
            .unwrap();
        #[allow(clippy::unwrap_used)]
        let attempt_a = inner.redeem(&req.id, principal.clone(), Vec::new()).unwrap();
        // Normal API usage can never reserve a second live attempt against
        // one grant (`AttemptInFlight` blocks it) — reach past that guard
        // directly to prove the settlement-side invariant check exists
        // independently, as its own defense.
        {
            let mut guard = inner.lock();
            if let Some(chain) = guard.chains.get_mut(&req.id) {
                chain.live_attempt = None;
            }
        }
        #[allow(clippy::unwrap_used)]
        let attempt_b = inner.redeem(&req.id, principal.clone(), Vec::new()).unwrap();
        assert!(inner.settle(&req.id, attempt_a, Outcome::Exit(0)).unwrap_or(false));
        // This settle call `debug_assert!`s and panics under the standard
        // debug test profile (`cargo test --all`), matching spec §B.3's
        // "a kernel bug ... panics in debug" — this test asserts the panic,
        // not a returned `Err` (which is what a release build would see
        // instead, since `debug_assert!` compiles out there).
        let _ = inner.settle(&req.id, attempt_b, Outcome::Exit(0));
    }

    #[test]
    #[should_panic(expected = "never reserved against this request")]
    fn settle_with_an_unreserved_attempt_id_is_invariant_violated() {
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(LedgerConfig::default(), None, Arc::new(SystemWallClock)).unwrap();
        let principal = agent("agent-1");
        #[allow(clippy::unwrap_used)]
        let req = inner
            .post_request(draft("fs.remove"), principal.clone(), Capture::DirectExecution, RequestContext::default(), Duration::from_secs(60), None)
            .unwrap();
        let not_after = SystemTime::now() + Duration::from_secs(300);
        #[allow(clippy::unwrap_used)]
        inner
            .grant(&req.id, GrantTerms::once_for(&req, not_after), principal, Grounds::Embedder)
            .unwrap();
        let bogus_attempt = AttemptId::new(999_999);
        let _ = inner.settle(&req.id, bogus_attempt, Outcome::Exit(0));
    }

    #[test]
    fn recovery_sweep_closes_an_unreported_reservation_as_abandoned() {
        let config = LedgerConfig {
            attempt_stale_after: Duration::from_millis(0),
            ..Default::default()
        };
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(config, None, Arc::new(SystemWallClock)).unwrap();
        let principal = agent("agent-1");
        #[allow(clippy::unwrap_used)]
        let req = inner
            .post_request(draft("fs.remove"), principal.clone(), Capture::DirectExecution, RequestContext::default(), Duration::from_secs(60), None)
            .unwrap();
        let not_after = SystemTime::now() + Duration::from_secs(300);
        #[allow(clippy::unwrap_used)]
        inner
            .grant(&req.id, GrantTerms::once_for(&req, not_after), principal.clone(), Grounds::Embedder)
            .unwrap();
        #[allow(clippy::unwrap_used)]
        let attempt = inner.redeem(&req.id, principal.clone(), Vec::new()).unwrap();

        // Let real monotonic time actually advance past the (zero) bound.
        std::thread::sleep(Duration::from_millis(5));
        inner.sweep();

        #[allow(clippy::unwrap_used)]
        let chain = inner.chain(&req.id).unwrap();
        #[allow(clippy::unwrap_used)]
        let record = chain.attempts.iter().find(|a| a.attempt == attempt).unwrap();
        assert!(matches!(record.state, AttemptState::Abandoned));

        // The chain closed as a side effect — a fresh redemption reports
        // "already settled" (spec §B.2: an abandoned attempt's effects are
        // unknown, same as `Outcome::Unknown`) rather than reserving a new
        // attempt against a grant nobody can vouch for.
        let err = inner.redeem(&req.id, principal, Vec::new()).unwrap_err();
        assert!(matches!(err, LedgerError::AlreadySettled { .. }));
    }

    /// Regression test for a review finding: `settle()` must not run
    /// `mark_closed` a second time when the chain already closed a
    /// different way (voided, expired, abandoned) while its attempt was
    /// still `Reserved`. A double-decrement of `live_count_total` would let
    /// the ledger admit more live requests than `live_capacity` — proven
    /// here through the public capacity gate itself rather than by reaching
    /// into `live_count_total` directly, so the test still means something
    /// if the counter's representation ever changes.
    #[test]
    fn settle_after_a_different_close_does_not_admit_extra_live_requests_past_capacity() {
        let config = LedgerConfig {
            live_capacity: 1,
            ..Default::default()
        };
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(config, None, Arc::new(SystemWallClock)).unwrap();
        let principal = agent("agent-1");

        // Chain A occupies the ledger's one live slot.
        #[allow(clippy::unwrap_used)]
        let req_a = inner
            .post_request(draft("fs.remove"), principal.clone(), Capture::DirectExecution, RequestContext::default(), Duration::from_secs(60), None)
            .unwrap();
        let not_after = SystemTime::now() + Duration::from_secs(300);
        #[allow(clippy::unwrap_used)]
        inner
            .grant(&req_a.id, GrantTerms::once_for(&req_a, not_after), principal.clone(), Grounds::Embedder)
            .unwrap();
        #[allow(clippy::unwrap_used)]
        let attempt_a = inner.redeem(&req_a.id, principal.clone(), Vec::new()).unwrap();

        // Void chain A via 5 bad keys while its attempt is still `Reserved`
        // — this closes the chain (freeing its live slot) without settling
        // the attempt.
        for _ in 0..5 {
            let _ = inner.redeem_with_token(&req_a.id, "wrong", principal.clone(), Vec::new());
        }
        assert_eq!(inner.state(&req_a.id), Some(RequestState::Voided));

        // The freed slot admits chain B, which stays live (undecided).
        #[allow(clippy::unwrap_used)]
        let _req_b = inner
            .post_request(draft("fs.remove"), principal.clone(), Capture::DirectExecution, RequestContext::default(), Duration::from_secs(60), None)
            .unwrap();

        // The now-orphaned attempt against already-voided chain A finally
        // settles. Before the fix, this called `mark_closed` for chain A a
        // second time, decrementing `live_count_total` again even though
        // chain B — not chain A — is what is actually occupying the slot.
        let _ = inner.settle(&req_a.id, attempt_a, Outcome::Exit(0));

        // If the double-decrement happened, the ledger now believes it has
        // 0 live requests even though chain B genuinely is one, and this
        // wrongly succeeds past the configured capacity of 1.
        let err = inner
            .post_request(draft("fs.remove"), principal, Capture::DirectExecution, RequestContext::default(), Duration::from_secs(60), None)
            .unwrap_err();
        assert!(
            matches!(err, LedgerError::LiveCapacity { limit: 1 }),
            "chain B is still live — the capacity gate must still refuse a third request, got {err:?}"
        );
    }
}
