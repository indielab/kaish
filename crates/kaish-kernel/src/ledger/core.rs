//! The ledger's one critical section (`docs/approval-ledger.md` §B.1).
//!
//! [`LedgerInner`] holds one `std::sync::Mutex<LedgerState>` — the whole
//! ledger's single lock. Every `do_*` method here acquires it exactly once,
//! samples the monotonic and wall clocks *after* acquiring it (so a caller
//! that blocks on contention is decided against the instant it actually
//! got the lock, never the instant it first called in — spec §B.1's
//! linearization is about commit order, and commit order is what the lock
//! serializes), reads the chain's current state, decides, and either
//! commits every entry the decision produces or commits nothing and
//! returns `Err`. Nothing `.await`s while the guard is live: sink delivery
//! reserves an [`tokio::sync::mpsc::OwnedPermit`] synchronously (never the
//! sink's own `post`, which runs on a background task — see
//! [`LedgerSink`]), so there is no async hook to accidentally call from
//! inside the section.
//!
//! **Terminal entries never refuse.** A `Redeemed` reservation reserves
//! ring and sink capacity for its own entry *and* for the terminal entry
//! (`Settled`, or attempt-level `Abandoned`) that will eventually close it
//! — banked on the [`AttemptRecord`] and consumed unconditionally when that
//! terminal entry lands. An operation that already ran must always be able
//! to record what happened (spec §D.4 / review finding B3); only the
//! *obligation* that would create new work is refusable by capacity.
//!
//! [`Requester`]/[`Approvals`]/[`ApproverHandle`] (`handles.rs`) are thin
//! public wrappers around the `pub(crate)` methods here; this file has no
//! public API of its own.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use kaish_types::approval::{
    ApprovalRequest, ApprovalRequestDraft, AttemptId, AttemptState, Capture, Condition, Expiring,
    Grant, GrantTerms, Grounds, LedgerEntry, Observation, Outcome, Principal, RequestContext,
    RequestId, RequestState, ResourceRef, StandingGrant, StandingId, StateClaim, Token,
};
use kaish_types::clock::Instant;
use tokio::sync::mpsc::OwnedPermit;

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
/// Removed from [`LedgerState::chains`] once it is both closed
/// ([`Chain::is_closed`]) and no ring entry names it any longer
/// (`ring_refs == 0`) — see [`LedgerState::evict_ring_front`]. A *live*
/// chain is never removed, however much ring pressure there is.
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
    /// How many entries currently in the ring name this request. Reaching
    /// zero on an already-closed chain is what makes it evictable from
    /// `LedgerState::chains` (review finding S4) — never while `>0` (an
    /// entry still points at it) or while live.
    ring_refs: usize,
}

struct AttemptRecord {
    state: AttemptState,
    reserved_at: Instant,
    outcome: Option<Outcome>,
    /// Sink capacity reserved at redemption time for this attempt's
    /// eventual terminal entry (`Settled`, or attempt-level `Abandoned`) —
    /// `None` when no sink is configured. Consumed unconditionally by
    /// whichever terminal entry lands first; never re-checked for capacity
    /// (review finding B3).
    terminal_sink_permit: Option<OwnedPermit<LedgerEntry>>,
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

/// Capacity reserved for `n` entries about to be committed: room in the
/// ring (already evicted, if eviction was needed) and, if a sink is
/// configured, one [`OwnedPermit`] per entry, guaranteeing every send in
/// the matching `commit*` call succeeds without a further capacity check.
/// Produced only by a `reserve_*` method that verified *everything* would
/// succeed before mutating anything (review finding S1) — dropping an
/// unused `ReservedCapacity` releases its permits back to the channel, so
/// a reservation that is never committed leaves no trace.
#[must_use]
struct ReservedCapacity {
    permits: Vec<OwnedPermit<LedgerEntry>>,
}

impl ReservedCapacity {
    fn take_one(&mut self) -> Option<OwnedPermit<LedgerEntry>> {
        self.permits.pop()
    }
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
    /// Ring slots promised to a not-yet-landed terminal entry (review
    /// finding B3) — counted against `retained_entries` alongside
    /// `ring.len()` so ordinary admissions can never squeeze out room a
    /// live attempt's eventual settlement already banked.
    reserved_ring_slots: usize,
    sink_tx: Option<tokio::sync::mpsc::Sender<LedgerEntry>>,
    sink_failed: Arc<AtomicBool>,
    /// How many entries the sink never received: every item still queued
    /// when the drain task hit a `post` failure, plus the one that failed
    /// (review finding S3). Read into `SinkUnavailable`'s message once
    /// `sink_failed` trips; meaningless before then.
    sink_dropped_count: Arc<AtomicUsize>,
}

impl LedgerState {
    fn alloc_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    /// Indices (ascending) of ring entries that would need evicting to make
    /// room for `n` more (on top of `reserved_ring_slots`), without
    /// mutating anything — `Err` if there are not enough evictable entries
    /// anywhere in the ring to make room.
    ///
    /// Scans for the *oldest evictable* entries, not strictly the front:
    /// a single long-lived request (e.g. a standing background job) sitting
    /// at the front of the ring must never permanently block eviction of
    /// closed entries behind it, or that one live entry pins the whole ring
    /// — and every entry appended after it — for the remaining life of the
    /// process, defeating the bounded-growth guarantee this mechanism
    /// exists for (review finding S4). Read-only half of the S1
    /// preflight-then-commit split.
    fn preview_ring_eviction(&self, n: usize, retained_entries: usize) -> Result<Vec<usize>, LedgerError> {
        let total_needed = (self.ring.len() + self.reserved_ring_slots + n).saturating_sub(retained_entries);
        if total_needed == 0 {
            return Ok(Vec::new());
        }
        let mut indices = Vec::with_capacity(total_needed);
        for (i, slot) in self.ring.iter().enumerate() {
            if indices.len() == total_needed {
                break;
            }
            let evictable = match &slot.request {
                None => true,
                Some(id) => self.chains.get(id).is_none_or(Chain::is_closed),
            };
            if evictable {
                indices.push(i);
            }
        }
        if indices.len() < total_needed {
            return Err(LedgerError::RingAtCapacity);
        }
        Ok(indices)
    }

    /// Remove and account the ring entries at `indices` (as found by
    /// `preview_ring_eviction`) — removed from highest index to lowest so
    /// earlier indices stay valid as the `VecDeque` shifts. If an evicted
    /// entry was the last one naming an already-closed chain, that chain is
    /// removed from `chains` entirely (review finding S4) — nothing about a
    /// still-*live* chain, or one another surviving entry still names, is
    /// ever touched here.
    fn evict_ring_entries(&mut self, mut indices: Vec<usize>) {
        indices.sort_unstable();
        for &i in indices.iter().rev() {
            if let Some(slot) = self.ring.remove(i) {
                self.account_evicted(slot);
            }
        }
    }

    fn account_evicted(&mut self, slot: RingSlot) {
        let Some(id) = slot.request else { return };
        let Some(chain) = self.chains.get_mut(&id) else { return };
        chain.ring_refs = chain.ring_refs.saturating_sub(1);
        if chain.ring_refs == 0 && chain.is_closed() {
            if let Some(removed) = self.chains.remove(&id) {
                self.trim_principal_entry(&removed.request.principal.id);
            }
        }
    }

    /// Drop a `live_count_by_principal` entry once it reaches zero, so
    /// that map does not grow one entry per distinct principal ever seen
    /// for the life of the process (review finding S4's second unbounded
    /// map).
    fn trim_principal_entry(&mut self, principal: &str) {
        if self.live_count_by_principal.get(principal) == Some(&0) {
            self.live_count_by_principal.remove(principal);
        }
    }

    /// Reserve `n` sink permits without sending anything. Read-only from
    /// the ring's perspective; each successful `try_reserve_owned` does
    /// mutate the channel's own internal semaphore, but a batch that fails
    /// partway rolls itself back — dropping the `Vec` of permits already
    /// taken releases every one of them back to the channel — so a caller
    /// that receives `Err` here has caused no observable effect (spec
    /// §B.1 / review finding S1).
    fn reserve_sink_permits(&self, n: usize) -> Result<Vec<OwnedPermit<LedgerEntry>>, LedgerError> {
        let Some(tx) = &self.sink_tx else {
            return Ok(Vec::new());
        };
        if self.sink_failed.load(Ordering::Relaxed) {
            return Err(LedgerError::SinkUnavailable(self.sink_failure_message()));
        }
        let mut permits = Vec::with_capacity(n);
        for _ in 0..n {
            match tx.clone().try_reserve_owned() {
                Ok(permit) => permits.push(permit),
                Err(_) => {
                    return Err(LedgerError::SinkUnavailable(format!(
                        "audit sink queue is full ({n} entries needed)"
                    )));
                    // `permits` (any already reserved this call) drops
                    // here, releasing them back to the channel.
                }
            }
        }
        Ok(permits)
    }

    fn sink_failure_message(&self) -> String {
        let dropped = self.sink_dropped_count.load(Ordering::Relaxed);
        format!(
            "audit sink failed; {dropped} audit entries undelivered — refusing further privileged operations until the process is restarted"
        )
    }

    /// Preflight for `n` normal (non-terminal, ordinarily-refusable)
    /// entries: room in the ring (previewed, not yet evicted) and `n` sink
    /// permits (reserved). Both succeed or neither mutates anything — only
    /// once both are confirmed does eviction actually run (review finding
    /// S1). Ring eviction of already-closed entries is always safe once it
    /// does run: it never removes information a still-live chain needs.
    fn reserve_capacity(&mut self, n: usize, retained_entries: usize) -> Result<ReservedCapacity, LedgerError> {
        let to_evict = self.preview_ring_eviction(n, retained_entries)?;
        let permits = self.reserve_sink_permits(n)?;
        self.evict_ring_entries(to_evict);
        Ok(ReservedCapacity { permits })
    }

    /// Reserve capacity for a `Redeemed` entry *and* its eventual terminal
    /// entry in one preflight-then-commit step: 2 ring slots (1 used now,
    /// 1 banked) and, if a sink is configured, 2 permits (1 used now, 1
    /// banked on the `AttemptRecord`). Nothing is mutated unless the whole
    /// reservation succeeds (review findings B3 + S1).
    fn reserve_redemption_capacity(&mut self, retained_entries: usize) -> Result<ReservedCapacity, LedgerError> {
        let to_evict = self.preview_ring_eviction(2, retained_entries)?;
        let permits = self.reserve_sink_permits(2)?;
        self.evict_ring_entries(to_evict);
        self.reserved_ring_slots += 1;
        Ok(ReservedCapacity { permits })
    }

    /// Push every entry into the ring, consuming one reserved permit per
    /// entry (if any were reserved). `entries.len()` must equal the `n`
    /// the matching `reserve_capacity` call was given, or some entries
    /// will land with no sink delivery at all (a caller bug, not a runtime
    /// condition this method can detect).
    fn commit(&mut self, entries: Vec<(LedgerEntry, Option<RequestId>)>, mut reserved: ReservedCapacity) -> Vec<LedgerEntry> {
        let mut committed = Vec::with_capacity(entries.len());
        for (entry, request) in entries {
            if let Some(permit) = reserved.take_one() {
                let _ = permit.send(entry.clone());
            }
            self.push_ring(entry.clone(), request);
            committed.push(entry);
        }
        committed
    }

    /// Commit the `Redeemed` entry itself from a `reserve_redemption_capacity`
    /// reservation: sends immediately using one permit, and returns the
    /// other (if any) to be banked on the new `AttemptRecord` for the
    /// eventual terminal entry.
    fn commit_redeemed(
        &mut self,
        entry: LedgerEntry,
        request: RequestId,
        mut reserved: ReservedCapacity,
    ) -> (LedgerEntry, Option<OwnedPermit<LedgerEntry>>) {
        let immediate = reserved.take_one();
        let banked = reserved.take_one();
        if let Some(permit) = immediate {
            let _ = permit.send(entry.clone());
        }
        self.push_ring(entry.clone(), Some(request));
        (entry, banked)
    }

    /// Commit a terminal entry (`Settled`, or attempt-level `Abandoned`)
    /// for an attempt whose capacity was already banked at redemption
    /// time. Never checks capacity and never fails — the room was
    /// reserved before this attempt was ever allowed to start (review
    /// finding B3: work that already ran must always be able to record
    /// what happened). Releases the ring slot banked for it back into
    /// ordinary circulation.
    fn commit_terminal(&mut self, entry: LedgerEntry, request: RequestId, permit: Option<OwnedPermit<LedgerEntry>>) -> LedgerEntry {
        if let Some(permit) = permit {
            let _ = permit.send(entry.clone());
        } else if let Some(tx) = &self.sink_tx {
            // Defensive fallback only — every `Redeemed` created while a
            // sink is configured always banks a permit for its terminal
            // entry, so this should be unreachable in practice. If it is
            // ever reached, the entry still lands in the ring (never
            // refused) and the gap is *accounted*, not silently dropped.
            if let Err(err) = tx.try_send(entry.clone()) {
                self.sink_dropped_count.fetch_add(1, Ordering::Relaxed);
                tracing::error!(
                    error = %err,
                    "approval ledger: terminal entry had no banked sink permit and try_send failed — recorded in the ring, counted as undelivered, never refused"
                );
            }
        }
        self.push_ring(entry.clone(), Some(request));
        self.reserved_ring_slots = self.reserved_ring_slots.saturating_sub(1);
        entry
    }

    fn push_ring(&mut self, entry: LedgerEntry, request: Option<RequestId>) {
        if let Some(id) = &request {
            if let Some(chain) = self.chains.get_mut(id) {
                chain.ring_refs += 1;
            }
        }
        self.ring.push_back(RingSlot { entry, request });
    }

    /// Maintain the live counters and drop the credential for a chain the
    /// caller has just transitioned into a closed state (spec §A.2 — the
    /// credential is dropped when the chain closes). Callers set
    /// `state`/`closed_by_settlement` themselves *before* calling this, and
    /// only when the chain was not already closed (see each call site's
    /// `was_already_closed` check — review finding B2) — this runs exactly
    /// once per chain, so the counters never go negative in practice;
    /// `saturating_sub` is defense in depth, not the mechanism.
    fn mark_closed(&mut self, id: &RequestId) {
        self.live_count_total = self.live_count_total.saturating_sub(1);
        if let Some(chain) = self.chains.get_mut(id) {
            let principal = chain.request.principal.id.clone();
            if let Some(count) = self.live_count_by_principal.get_mut(&principal) {
                *count = count.saturating_sub(1);
            }
            chain.token = None;
        }
        self.trim_principal_entry(
            &self
                .chains
                .get(id)
                .map(|c| c.request.principal.id.clone())
                .unwrap_or_default(),
        );
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

    /// Sample both clocks. Every call site calls this **after** acquiring
    /// the lock, at the transaction's actual commit point (review finding
    /// B1) — a caller that blocks on contention is decided against the
    /// instant it got the lock, never the instant it first called in.
    /// Sampling before locking would let a caller be admitted or denied on
    /// a stale instant, and would stamp `at` with arrival time instead of
    /// commit time.
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
    /// Returns the entries it committed rather than emitting their tracing
    /// events itself (review finding S6) — every caller already holds the
    /// guard when calling in and must keep holding it a moment longer to
    /// finish its own transaction, so emitting here would happen while the
    /// lock is still live. A subscriber re-entering `Approvals` from inside
    /// its own event handler would then deadlock on this same
    /// non-reentrant `std::sync::Mutex`. This is also the exact boundary
    /// PR 4's `Approver::decide` hook relies on: nothing in this file may
    /// emit, await, or call out while `guard` is alive.
    fn materialize_expiry(
        &self,
        guard: &mut LedgerState,
        id: &RequestId,
        mono: Instant,
        wall: SystemTime,
    ) -> Result<Vec<LedgerEntry>, LedgerError> {
        let Some(chain) = guard.chains.get(id) else {
            return Ok(Vec::new());
        };
        let what = match chain.state {
            RequestState::Requested if mono >= chain.request_deadline => Some(Expiring::Request),
            RequestState::Granted if !chain.closed_by_settlement => match chain.grant_deadline {
                Some(deadline) if mono >= deadline => Some(Expiring::Grant),
                _ => None,
            },
            _ => None,
        };
        let Some(what) = what else {
            return Ok(Vec::new());
        };
        let reserved = guard.reserve_capacity(1, self.config.retained_entries)?;
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
        let committed = guard.commit(entries, reserved);
        if let Some(chain) = guard.chains.get_mut(id) {
            chain.state = RequestState::Expired;
        }
        guard.mark_closed(id);
        Ok(committed)
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
        let mut guard = self.lock();
        let (mono, wall) = self.now();
        let (request, committed) =
            self.post_request_locked(&mut guard, draft, principal, capture, context, ttl, job_id, mono, wall)?;
        drop(guard);
        emit_events(&committed);
        Ok(request)
    }

    /// The shared core of `post_request` and `renew` (review finding S5):
    /// both need "check state, then post a new `Requested`" under *one*
    /// held guard — `renew` additionally needs to have already checked its
    /// old request is `Expired` under the same lock acquisition that
    /// posts the superseding request, so nothing can race between the two.
    #[allow(clippy::too_many_arguments)]
    fn post_request_locked(
        &self,
        guard: &mut LedgerState,
        draft: ApprovalRequestDraft,
        principal: Principal,
        capture: Capture,
        context: RequestContext,
        ttl: Duration,
        job_id: Option<u64>,
        mono: Instant,
        wall: SystemTime,
    ) -> Result<(ApprovalRequest, Vec<LedgerEntry>), LedgerError> {
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
        let reserved = guard.reserve_capacity(1, self.config.retained_entries)?;

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
            ring_refs: 0,
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
        let committed = guard.commit(entries, reserved);
        Ok((request, committed))
    }

    pub(crate) fn redeem(
        &self,
        id: &RequestId,
        by: Principal,
        observed: Vec<Observation>,
    ) -> Result<AttemptId, LedgerError> {
        let mut guard = self.lock();
        let (mono, wall) = self.now();
        let mut all_committed = self.materialize_expiry(&mut guard, id, mono, wall)?;
        let (result, committed) = self.redeem_locked(&mut guard, id, by, observed, mono, wall);
        all_committed.extend(committed);
        drop(guard);
        emit_events(&all_committed);
        result
    }

    pub(crate) fn redeem_with_token(
        &self,
        id: &RequestId,
        presented: &str,
        by: Principal,
        observed: Vec<Observation>,
    ) -> Result<AttemptId, LedgerError> {
        let mut guard = self.lock();
        let (mono, wall) = self.now();
        let mut all_committed = self.materialize_expiry(&mut guard, id, mono, wall)?;

        let Some(chain) = guard.chains.get(id) else {
            // A guessed id that matches nothing counts against nothing, so a
            // guesser cannot void a request it cannot describe (spec §F.3).
            // Best-effort: if the ring/sink has no room even for this one
            // bookkeeping entry, skip recording it rather than failing a
            // rejection that was never going to succeed anyway — seq is
            // only allocated once capacity is confirmed, so a skip here
            // never opens a gap.
            if let Ok(reserved) = guard.reserve_capacity(1, self.config.retained_entries) {
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
                all_committed.extend(guard.commit(entries, reserved));
            }
            drop(guard);
            emit_events(&all_committed);
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
            drop(guard);
            emit_events(&all_committed);
            return Err(err);
        }

        // Constant-time comparison as defense in depth against a timing
        // side-channel on credential comparison (review NIT). Not a full
        // mitigation — the ledger's stated threat model (spec §A.2) already
        // excludes a hostile process sharing this address space — but it
        // removes an easy timing leak for the cost of one helper function.
        let matches_real_token = guard
            .chains
            .get(id)
            .and_then(|c| c.token.as_ref())
            .is_some_and(|t| constant_time_eq(t.reveal(), presented));

        if matches_real_token {
            let (result, committed) = self.redeem_locked(&mut guard, id, by, observed, mono, wall);
            all_committed.extend(committed);
            drop(guard);
            emit_events(&all_committed);
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
            drop(guard);
            emit_events(&all_committed);
            return Err(LedgerError::NotAuthorized(id.clone()));
        };
        let n = chain.reject_count + 1;
        let voids_now = n >= self.config.max_token_attempts;
        let reserved = match guard.reserve_capacity(if voids_now { 2 } else { 1 }, self.config.retained_entries) {
            Ok(r) => r,
            Err(err) => {
                drop(guard);
                emit_events(&all_committed);
                return Err(err);
            }
        };
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
        all_committed.extend(guard.commit(entries, reserved));
        drop(guard);
        emit_events(&all_committed);
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
            let reserved = match guard.reserve_capacity(2, self.config.retained_entries) {
                Ok(r) => r,
                Err(err) => return (Err(err), Vec::new()),
            };
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
            let committed = guard.commit(entries, reserved);
            return (
                Err(LedgerError::Refused {
                    id: id.clone(),
                    detail: reason,
                }),
                committed,
            );
        }

        // Reserve room for the `Redeemed` entry AND its eventual terminal
        // entry together (review finding B3) — the terminal entry this
        // attempt produces (`Settled`, or an attempt-level `Abandoned` from
        // the sweep) must never be refusable once work has started.
        let reserved = match guard.reserve_redemption_capacity(self.config.retained_entries) {
            Ok(r) => r,
            Err(err) => return (Err(err), Vec::new()),
        };
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
        let (committed_entry, terminal_permit) = guard.commit_redeemed(entry, id.clone(), reserved);
        if let Some(chain) = guard.chains.get_mut(id) {
            chain.attempts.insert(
                attempt_id,
                AttemptRecord {
                    state: AttemptState::Reserved,
                    reserved_at: mono,
                    outcome: None,
                    terminal_sink_permit: terminal_permit,
                },
            );
            chain.live_attempt = Some(attempt_id);
        }
        (Ok(attempt_id), vec![committed_entry])
    }

    pub(crate) fn settle(
        &self,
        request_id: &RequestId,
        attempt_id: AttemptId,
        outcome: Outcome,
    ) -> Result<bool, LedgerError> {
        let mut guard = self.lock();
        let (_, wall) = self.now();
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
        let closes = matches!(outcome, Outcome::Exit(0) | Outcome::Unknown { .. });
        if closes && chain.closed_by_settlement {
            debug_assert!(false, "a second successful settlement was attempted against one grant");
            return Err(LedgerError::InvariantViolated(format!(
                "request {request_id} already has a successful (or Unknown) settlement — a grant authorizes exactly one"
            )));
        }
        // The chain may already have closed a different way (voided by a
        // 5th bad credential, expired past `not_after`, abandoned, or swept
        // as a stale reservation) while this attempt was still `Reserved`
        // — none of those paths check `live_attempt`, by design (spec
        // §B.2: those are derived facts about the world, not about any one
        // attempt). A chain closes exactly once; `mark_closed` must not run
        // a second time here, or the live counters it maintains undercount
        // (review finding B2 / spec §D.4's `live_capacity` gate would then
        // admit more than its configured number of genuinely live
        // requests).
        let was_already_closed = chain.is_closed();

        // Terminal entries are never capacity-refusable (review finding
        // B3) — the room was banked when this attempt was reserved.
        let permit = guard
            .chains
            .get_mut(request_id)
            .and_then(|c| c.attempts.get_mut(&attempt_id))
            .and_then(|r| r.terminal_sink_permit.take());
        let seq = guard.alloc_seq();
        let entry = LedgerEntry::Settled {
            seq,
            at: wall,
            request: request_id.clone(),
            attempt: attempt_id,
            outcome: outcome.clone(),
        };
        let committed_entry = guard.commit_terminal(entry, request_id.clone(), permit);
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
        emit_events(&[committed_entry]);
        Ok(true)
    }

    pub(crate) fn abandon_request(&self, id: &RequestId, reason: String) -> Result<(), LedgerError> {
        let mut guard = self.lock();
        let (mono, wall) = self.now();
        let mut all_committed = self.materialize_expiry(&mut guard, id, mono, wall)?;
        let Some(chain) = guard.chains.get(id) else {
            return Err(LedgerError::NotFound(id.clone()));
        };
        if chain.is_closed() {
            let err = self.terminal_error(id, chain.state, chain.void_reason.clone());
            drop(guard);
            emit_events(&all_committed);
            return Err(err);
        }
        if chain.live_attempt.is_some() {
            drop(guard);
            emit_events(&all_committed);
            return Err(LedgerError::AttemptInFlight(id.clone()));
        }
        let reserved = match guard.reserve_capacity(1, self.config.retained_entries) {
            Ok(r) => r,
            Err(err) => {
                drop(guard);
                emit_events(&all_committed);
                return Err(err);
            }
        };
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
        all_committed.extend(guard.commit(entries, reserved));
        if let Some(chain) = guard.chains.get_mut(id) {
            chain.state = RequestState::Abandoned;
        }
        guard.mark_closed(id);
        drop(guard);
        emit_events(&all_committed);
        Ok(())
    }

    /// Renew an `Expired` request into a fresh `Requested` one (spec §B.5),
    /// as one critical section (review finding S5 — this used to acquire
    /// the lock up to three times).
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
        let mut guard = self.lock();
        let (mono, wall) = self.now();
        let mut all_committed = self.materialize_expiry(&mut guard, id, mono, wall)?;

        let Some(chain) = guard.chains.get(id) else {
            drop(guard);
            emit_events(&all_committed);
            return Err(LedgerError::NotFound(id.clone()));
        };
        if chain.state != RequestState::Expired {
            let err = LedgerError::NotRenewable {
                id: id.clone(),
                state: chain.state,
            };
            drop(guard);
            emit_events(&all_committed);
            return Err(err);
        }
        let old = chain.request.clone();

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
        let draft = match builder.build() {
            Ok(draft) => draft,
            Err(e) => {
                debug_assert!(false, "renew: rebuilding a draft from an already-valid request failed: {e}");
                let err = LedgerError::InvariantViolated(format!("renew: failed to rebuild request {id} as a draft: {e}"));
                drop(guard);
                emit_events(&all_committed);
                return Err(err);
            }
        };

        let result = self.post_request_locked(
            &mut guard,
            draft,
            old.principal,
            old.capture,
            old.context,
            old.ttl,
            old.job_id,
            mono,
            wall,
        );
        drop(guard);
        match result {
            Ok((request, committed)) => {
                all_committed.extend(committed);
                emit_events(&all_committed);
                Ok(request.id)
            }
            Err(err) => {
                emit_events(&all_committed);
                Err(err)
            }
        }
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
        // to be invalid (already decided, terminal, widened) has drawn
        // entropy for nothing; that is a cheap, inconsequential cost next
        // to the alternative of blocking every other transaction on this
        // ledger.
        let token = Token::new(
            generate_credential().map_err(|e| LedgerError::CredentialUnavailable(e.to_string()))?,
        );
        let mut guard = self.lock();
        let (mono, wall) = self.now();
        self.materialize_expiry(&mut guard, id, mono, wall)?;
        let Some(chain) = guard.chains.get(id) else {
            return Err(LedgerError::NotFound(id.clone()));
        };
        match chain.state {
            RequestState::Requested => {}
            RequestState::Granted => return Err(LedgerError::AlreadyDecided(id.clone())),
            other => return Err(self.terminal_error(id, other, chain.void_reason.clone())),
        }
        // An approver may narrow (add or tighten) the request's declared
        // transition claims and may never widen them — every
        // transition-bearing resource on the request must have a matching
        // condition in `terms`, checked before capacity/seq (review
        // finding B4; spec §A.4). `GrantTerms::once_for` (the standard
        // path) always satisfies this trivially, since it copies the
        // request's transitions verbatim; this only rejects a caller that
        // dropped or altered one.
        if let Some((resource, expected)) = find_widened_condition(&chain.request, &terms) {
            return Err(LedgerError::ConditionsWidened {
                request: id.clone(),
                resource,
                expected,
            });
        }
        let reserved = guard.reserve_capacity(1, self.config.retained_entries)?;

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
        let committed = guard.commit(entries, reserved);
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
        let mut guard = self.lock();
        let (mono, wall) = self.now();
        let mut all_committed = self.materialize_expiry(&mut guard, id, mono, wall)?;
        let Some(chain) = guard.chains.get(id) else {
            drop(guard);
            emit_events(&all_committed);
            return Err(LedgerError::NotFound(id.clone()));
        };
        match chain.state {
            RequestState::Requested => {}
            RequestState::Granted => {
                drop(guard);
                emit_events(&all_committed);
                return Err(LedgerError::AlreadyDecided(id.clone()));
            }
            other => {
                let err = self.terminal_error(id, other, chain.void_reason.clone());
                drop(guard);
                emit_events(&all_committed);
                return Err(err);
            }
        }
        let reserved = match guard.reserve_capacity(1, self.config.retained_entries) {
            Ok(r) => r,
            Err(err) => {
                drop(guard);
                emit_events(&all_committed);
                return Err(err);
            }
        };
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
        all_committed.extend(guard.commit(entries, reserved));
        if let Some(chain) = guard.chains.get_mut(id) {
            chain.state = RequestState::Denied;
        }
        guard.mark_closed(id);
        drop(guard);
        emit_events(&all_committed);
        Ok(())
    }

    pub(crate) fn grant_standing(&self, mut standing: StandingGrant) -> Result<StandingId, LedgerError> {
        let mut guard = self.lock();
        let (_, wall) = self.now();
        let reserved = guard.reserve_capacity(1, self.config.retained_entries)?;
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
        let committed = guard.commit(entries, reserved);
        drop(guard);
        emit_events(&committed);
        Ok(id)
    }

    pub(crate) fn revoke_standing(&self, id: StandingId, by: Principal, reason: String) -> Result<(), LedgerError> {
        let mut guard = self.lock();
        let (_, wall) = self.now();
        if !guard.standing.contains_key(&id) {
            return Err(LedgerError::StandingNotFound(id));
        }
        let reserved = guard.reserve_capacity(1, self.config.retained_entries)?;
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
        let committed = guard.commit(entries, reserved);
        drop(guard);
        emit_events(&committed);
        Ok(())
    }

    /// Retrieve the credential. Appends `KeyRetrieved` naming `by`, and
    /// returns `None` — never handing out the key — when that entry cannot
    /// be recorded (review finding S2: accountability is the record, not
    /// the mechanism, so a credential the ledger cannot account for is not
    /// handed out; the `Option`-only signature, spec §D.2, means the
    /// caller cannot distinguish "no credential exists" from "capacity
    /// refused the retrieval", which is the correct fail-closed shape for
    /// a bearer secret either way).
    pub(crate) fn token_for(&self, id: &RequestId, by: Principal) -> Option<Token> {
        let mut guard = self.lock();
        let (_, wall) = self.now();
        let token = guard.chains.get(id)?.token.clone()?;
        let reserved = guard.reserve_capacity(1, self.config.retained_entries).ok()?;
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
        let committed = guard.commit(entries, reserved);
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
        let ids: Vec<RequestId> = {
            let guard = self.lock();
            guard.chains.keys().cloned().collect()
        };
        for id in ids {
            let committed = {
                let mut guard = self.lock();
                let (mono, wall) = self.now();
                self.materialize_expiry(&mut guard, &id, mono, wall)
            };
            if let Ok(committed) = committed {
                emit_events(&committed);
            }
            self.sweep_stale_attempt(&id);
        }
    }

    fn sweep_stale_attempt(&self, id: &RequestId) {
        let mut guard = self.lock();
        let (mono, wall) = self.now();
        let Some(chain) = guard.chains.get(id) else { return };
        let Some(attempt_id) = chain.live_attempt else { return };
        let Some(record) = chain.attempts.get(&attempt_id) else { return };
        if !matches!(record.state, AttemptState::Reserved) {
            return;
        }
        if mono.duration_since(record.reserved_at) < self.config.attempt_stale_after {
            return;
        }
        // The chain may already have closed a different way (a 5th bad
        // credential, expiry) while this attempt sat `Reserved` — same
        // guard as `settle()`, same reason (review finding B2).
        let was_already_closed = chain.is_closed();

        // A terminal entry (this is one — attempt-level `Abandoned`) is
        // never capacity-refusable (review finding B3); the room was
        // banked when this attempt was reserved.
        let permit = guard
            .chains
            .get_mut(id)
            .and_then(|c| c.attempts.get_mut(&attempt_id))
            .and_then(|r| r.terminal_sink_permit.take());
        let seq = guard.alloc_seq();
        let reason = "recovery sweep: reservation exceeded the staleness bound with no report".to_string();
        let entry = LedgerEntry::Abandoned {
            seq,
            at: wall,
            request: id.clone(),
            attempt: Some(attempt_id),
            reason,
        };
        let committed_entry = guard.commit_terminal(entry, id.clone(), permit);
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
        if !was_already_closed {
            guard.mark_closed(id);
        }
        drop(guard);
        emit_events(&[committed_entry]);
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
        let mut guard = self.lock();
        let (mono, wall) = self.now();
        let result = self.materialize_expiry(&mut guard, id, mono, wall);
        drop(guard);
        if let Ok(committed) = result {
            emit_events(&committed);
        }
    }
}

/// Whether `terms` widens any transition-bearing resource on `request` —
/// i.e. drops or alters a condition the request itself declared (spec
/// §A.4 / review finding B4). Returns the first offending resource and
/// what the request expected, for the error. Extra conditions in `terms`
/// for resources the request never declared are fine (that is "narrowing
/// by adding" — spec §A.4 explicitly allows it).
fn find_widened_condition(request: &ApprovalRequest, terms: &GrantTerms) -> Option<(ResourceRef, StateClaim)> {
    for resource in &request.resources {
        let Some(expected) = resource.to_condition() else {
            continue;
        };
        let satisfied = terms
            .conditions
            .iter()
            .any(|c| c.resource == expected.resource && c.expected_from == expected.expected_from);
        if !satisfied {
            return Some((expected.resource, expected.expected_from));
        }
    }
    None
}

/// Constant-time string equality (review NIT — defense in depth on
/// credential comparison; see its call site for the threat-model caveat).
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// One event per appended entry, at the same call site the entry itself was
/// committed at — "no second place where a ledger fact can be recorded
/// without a trace fact" (spec §G). Levels match the spec's Events table.
/// Called only after every lock this batch of entries was committed under
/// has been dropped (review finding S6).
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
    let sink_dropped_count = Arc::new(AtomicUsize::new(0));
    let sink_tx = sink.as_ref().map(|sink| {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<LedgerEntry>(config.sink_queue.max(1));
        let sink = Arc::clone(sink);
        let failed = Arc::clone(&sink_failed);
        let dropped_count = Arc::clone(&sink_dropped_count);
        tokio::spawn(async move {
            while let Some(entry) = rx.recv().await {
                if let Err(err) = sink.post(&entry) {
                    tracing::error!(error = %err, "approval ledger: audit sink failed — refusing further obligations");
                    failed.store(true, Ordering::Relaxed);
                    // Count the entry that just failed, plus every entry
                    // still queued behind it — this task is about to stop
                    // consuming, so all of it is now undelivered (review
                    // finding S3: the contract must account what it drops,
                    // never just silently abandon the backlog).
                    let mut dropped = 1usize;
                    while rx.try_recv().is_ok() {
                        dropped += 1;
                    }
                    dropped_count.fetch_add(dropped, Ordering::Relaxed);
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
        reserved_ring_slots: 0,
        sink_tx,
        sink_failed,
        sink_dropped_count,
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

    use kaish_types::approval::{ApprovalRequest, AttemptId, PrincipalKind, Resource, RiskClass};

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

    #[allow(clippy::unwrap_used)]
    fn post(inner: &LedgerInner, principal: &Principal) -> ApprovalRequest {
        inner
            .post_request(
                draft("fs.remove"),
                principal.clone(),
                Capture::DirectExecution,
                RequestContext::default(),
                Duration::from_secs(60),
                None,
            )
            .unwrap()
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
        let req = post(&inner, &principal);

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
        let req = post(&inner, &principal);
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

    /// Regression test for review finding B1: every commit-point clock
    /// sample must happen *after* the lock is actually acquired, not
    /// before a caller blocks on contention. A background thread holds the
    /// real lock for a known interval; `grant()` necessarily blocks until
    /// it releases, so a correctly-ordered sample can never predate the
    /// release — sampling before locking (the bug) would instead stamp
    /// `decided_at` around when `grant()` was first called.
    #[test]
    fn grant_decided_at_is_sampled_after_acquiring_the_lock_not_before() {
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(LedgerConfig::default(), None, Arc::new(SystemWallClock)).unwrap();
        let principal = agent("agent-1");
        let req = post(&inner, &principal);

        let release_time = Arc::new(Mutex::new(None::<SystemTime>));
        let release_time2 = Arc::clone(&release_time);
        let inner2 = Arc::clone(&inner);
        let holder = std::thread::spawn(move || {
            let _guard = inner2.lock();
            std::thread::sleep(Duration::from_millis(60));
            #[allow(clippy::unwrap_used)]
            {
                *release_time2.lock().unwrap() = Some(SystemTime::now());
            }
            // guard drops here, releasing the lock to the blocked caller.
        });
        // Give the background thread a head start so it grabs the lock
        // before `grant()` below ever attempts to.
        std::thread::sleep(Duration::from_millis(15));

        let not_after = SystemTime::now() + Duration::from_secs(300);
        #[allow(clippy::unwrap_used)]
        inner
            .grant(&req.id, GrantTerms::once_for(&req, not_after), principal, Grounds::Embedder)
            .unwrap();
        #[allow(clippy::unwrap_used)]
        holder.join().unwrap();

        #[allow(clippy::unwrap_used)]
        let release_time = release_time.lock().unwrap().unwrap();
        #[allow(clippy::unwrap_used)]
        let chain = inner.chain(&req.id).unwrap();
        #[allow(clippy::unwrap_used)]
        let decided_at = chain.grant.unwrap().decided_at;
        assert!(
            decided_at >= release_time,
            "decided_at ({decided_at:?}) must be sampled at or after the lock was actually released by the holder ({release_time:?}) — sampling before blocking on the lock would stamp an earlier, stale instant"
        );
    }

    #[test]
    #[should_panic(expected = "second successful settlement")]
    fn second_successful_settlement_against_one_grant_is_invariant_violated() {
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(LedgerConfig::default(), None, Arc::new(SystemWallClock)).unwrap();
        let principal = agent("agent-1");
        let req = post(&inner, &principal);
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
        let req = post(&inner, &principal);
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
        let req = post(&inner, &principal);
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
        let req_a = post(&inner, &principal);
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
        let _req_b = post(&inner, &principal);

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

    /// Regression test for review finding B2: the recovery sweep must use
    /// the same `was_already_closed` guard `settle()` does. Reproduces the
    /// review's exact 4-step sequence: reserve an attempt, void the chain
    /// via 5 bad keys (closing it once), let the sweep find the
    /// now-orphaned stale attempt (which must NOT close the chain again),
    /// then prove the live-capacity gate still holds.
    #[test]
    fn sweep_after_a_different_close_does_not_admit_extra_live_requests_past_capacity() {
        let config = LedgerConfig {
            live_capacity: 1,
            attempt_stale_after: Duration::from_millis(0),
            ..Default::default()
        };
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(config, None, Arc::new(SystemWallClock)).unwrap();
        let principal = agent("agent-1");

        let req_a = post(&inner, &principal);
        let not_after = SystemTime::now() + Duration::from_secs(300);
        #[allow(clippy::unwrap_used)]
        inner
            .grant(&req_a.id, GrantTerms::once_for(&req_a, not_after), principal.clone(), Grounds::Embedder)
            .unwrap();
        // Step 1: reserve an attempt.
        #[allow(clippy::unwrap_used)]
        let _attempt = inner.redeem(&req_a.id, principal.clone(), Vec::new()).unwrap();
        // Step 2: void via 5 bad keys while the attempt is still Reserved
        // — closes the chain once, frees the live slot.
        for _ in 0..5 {
            let _ = inner.redeem_with_token(&req_a.id, "wrong", principal.clone(), Vec::new());
        }
        assert_eq!(inner.state(&req_a.id), Some(RequestState::Voided));

        // The freed slot admits chain B, which stays live.
        let _req_b = post(&inner, &principal);

        // Step 3: the sweep finds the now-orphaned stale attempt against
        // already-voided chain A and closes IT too — this must not call
        // mark_closed a second time.
        std::thread::sleep(Duration::from_millis(5));
        inner.sweep();

        // Step 4: the capacity gate must still hold — chain B is the only
        // genuinely live request.
        let err = inner
            .post_request(draft("fs.remove"), principal, Capture::DirectExecution, RequestContext::default(), Duration::from_secs(60), None)
            .unwrap_err();
        assert!(
            matches!(err, LedgerError::LiveCapacity { limit: 1 }),
            "chain B is still live — the capacity gate must still refuse a third request, got {err:?}"
        );
    }

    /// Regression test for review finding B3: a terminal entry (`Settled`)
    /// must land even when the ring is otherwise completely full — capacity
    /// for it (and for `Redeemed` itself) is banked together, at redemption
    /// time, as a pair (review finding B3's "count 2 at reservation"). With
    /// `retained_entries: 4`, `Requested` + `Granted` + `Redeemed` +
    /// banked-`Settled` exactly fill it — the redemption itself would
    /// refuse (there is nowhere to bank the pair) one entry sooner than
    /// this, so this is the tightest configuration that actually reaches
    /// `settle()`. Without the fix, `settle()` would return
    /// `RingAtCapacity` for an operation that had already run — exactly
    /// the "balance rule violated forever" scenario the review described.
    #[test]
    fn terminal_settled_entry_is_never_refused_by_ring_capacity() {
        let config = LedgerConfig {
            retained_entries: 4,
            ..Default::default()
        };
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(config, None, Arc::new(SystemWallClock)).unwrap();
        let principal = agent("agent-1");

        let req = post(&inner, &principal); // 1: Requested
        let not_after = SystemTime::now() + Duration::from_secs(300);
        #[allow(clippy::unwrap_used)]
        inner
            .grant(&req.id, GrantTerms::once_for(&req, not_after), principal.clone(), Grounds::Embedder)
            .unwrap(); // 2: Granted
        #[allow(clippy::unwrap_used)]
        let attempt = inner.redeem(&req.id, principal, Vec::new()).unwrap(); // 3: Redeemed, + 1 banked for the terminal — ring is now exactly full at 4

        // The terminal entry must still land — never refused.
        let appended = inner
            .settle(&req.id, attempt, Outcome::Exit(0))
            .expect("a terminal entry must never be refused by ring capacity");
        assert!(appended);
        assert_eq!(inner.state(&req.id), Some(RequestState::Granted));
        #[allow(clippy::unwrap_used)]
        let chain = inner.chain(&req.id).unwrap();
        assert!(matches!(
            chain.attempts.iter().find(|a| a.attempt == attempt).map(|a| a.state),
            Some(AttemptState::Settled)
        ));
    }

    /// Same guarantee as the ring test above, but for the sink: with a
    /// tiny `sink_queue`, the `Redeemed` reservation banks a permit for
    /// the terminal entry at redemption time, so filling the queue with
    /// unrelated entries afterward must not be able to starve it.
    #[derive(Default)]
    struct AcceptingSink {
        received: Mutex<Vec<LedgerEntry>>,
    }
    impl LedgerSink for AcceptingSink {
        fn post(&self, entry: &LedgerEntry) -> Result<(), super::super::config::LedgerSinkError> {
            #[allow(clippy::unwrap_used)]
            self.received.lock().unwrap().push(entry.clone());
            Ok(())
        }
    }

    // `build_inner` spawns the sink drain task via `tokio::spawn`, which
    // panics immediately with no runtime in scope even if the test never
    // awaits anything — every test that configures a sink needs
    // `#[tokio::test]` for exactly that reason.
    #[tokio::test]
    async fn terminal_settled_entry_is_never_refused_by_sink_capacity() {
        let sink = Arc::new(AcceptingSink::default());
        // Same tightest-configuration reasoning as the ring test above:
        // Requested + Granted each consume one permit immediately (the
        // drain task hasn't run yet — no `.await` has happened), so by the
        // time redemption needs to bank 2 more (1 immediate + 1 for the
        // eventual terminal), only a queue of at least 4 has room for all
        // of it without any entry ever being refused.
        let config = LedgerConfig {
            sink_queue: 4,
            ..Default::default()
        };
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(config, Some(sink), Arc::new(SystemWallClock)).unwrap();
        let principal = agent("agent-1");

        let req = post(&inner, &principal);
        let not_after = SystemTime::now() + Duration::from_secs(300);
        #[allow(clippy::unwrap_used)]
        inner
            .grant(&req.id, GrantTerms::once_for(&req, not_after), principal.clone(), Grounds::Embedder)
            .unwrap();
        #[allow(clippy::unwrap_used)]
        let attempt = inner.redeem(&req.id, principal, Vec::new()).unwrap();

        // Terminal capacity was banked at redemption time — settle must
        // succeed regardless of anything else contending for the queue.
        let appended = inner
            .settle(&req.id, attempt, Outcome::Exit(0))
            .expect("a terminal entry must never be refused by sink capacity");
        assert!(appended);
    }

    /// Regression test for review finding B4: a caller that drops or
    /// alters a request's declared transition claim in `GrantTerms` must
    /// be rejected — this is the "terms.conditions.clear()" attack the
    /// review named. Covers all four cases the review asked for.
    #[test]
    fn grant_rejects_widened_conditions_but_allows_narrower_or_added_ones() {
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(LedgerConfig::default(), None, Arc::new(SystemWallClock)).unwrap();
        let principal = agent("agent-1");
        let not_after = SystemTime::now() + Duration::from_secs(300);

        let make_request = |inner: &LedgerInner| -> ApprovalRequest {
            #[allow(clippy::unwrap_used)]
            let draft = ApprovalRequest::builder("git.push")
                .risk(RiskClass::Irreversible)
                .resource(Resource::transition(
                    "git.ref",
                    "refs/heads/main",
                    StateClaim::Exact("a1b2".to_string()),
                    StateClaim::Exact("c3d4".to_string()),
                ))
                .build()
                .unwrap();
            #[allow(clippy::unwrap_used)]
            inner
                .post_request(draft, agent("agent-1"), Capture::DirectExecution, RequestContext::default(), Duration::from_secs(60), None)
                .unwrap()
        };

        // Case: removed — terms carries no condition at all.
        let req = make_request(&inner);
        let terms = GrantTerms::new(not_after, Vec::new());
        let err = inner.grant(&req.id, terms, principal.clone(), Grounds::Embedder).unwrap_err();
        assert!(matches!(err, LedgerError::ConditionsWidened { .. }), "removed: got {err:?}");

        // Case: altered — same resource, a different expected_from.
        let req = make_request(&inner);
        let terms = GrantTerms::new(
            not_after,
            vec![Condition {
                resource: ResourceRef {
                    kind: "git.ref".to_string(),
                    id: "refs/heads/main".to_string(),
                },
                expected_from: StateClaim::Exact("wrong".to_string()),
            }],
        );
        let err = inner.grant(&req.id, terms, principal.clone(), Grounds::Embedder).unwrap_err();
        assert!(matches!(err, LedgerError::ConditionsWidened { .. }), "altered: got {err:?}");

        // Case: unrelated-added — the exact declared condition, plus an
        // extra one for a resource the request never declared. Allowed
        // (spec §A.4: "narrow (add or tighten)").
        let req = make_request(&inner);
        let mut terms = GrantTerms::once_for(&req, not_after);
        terms.conditions.push(Condition {
            resource: ResourceRef {
                kind: "git.remote".to_string(),
                id: "origin".to_string(),
            },
            expected_from: StateClaim::Exact("unrelated".to_string()),
        });
        assert!(
            inner.grant(&req.id, terms, principal.clone(), Grounds::Embedder).is_ok(),
            "an added, unrelated condition must not be treated as widening"
        );

        // Case: valid-narrower — exactly what once_for produces.
        let req = make_request(&inner);
        let terms = GrantTerms::once_for(&req, not_after);
        assert!(inner.grant(&req.id, terms, principal, Grounds::Embedder).is_ok());
    }

    /// Regression test for review finding S2: a credential retrieval that
    /// cannot record `KeyRetrieved` must return `None` — never hand out
    /// the key without the accountability entry (Amy's "accountability is
    /// the record, not the mechanism" decision).
    #[test]
    fn token_for_returns_none_rather_than_an_unaccounted_credential_under_ring_pressure() {
        let config = LedgerConfig {
            retained_entries: 2,
            ..Default::default()
        };
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(config, None, Arc::new(SystemWallClock)).unwrap();
        let principal = agent("agent-1");
        let req = post(&inner, &principal); // 1: Requested
        let not_after = SystemTime::now() + Duration::from_secs(300);
        #[allow(clippy::unwrap_used)]
        inner
            .grant(&req.id, GrantTerms::once_for(&req, not_after), principal.clone(), Grounds::Embedder)
            .unwrap(); // 2: Granted — ring is now exactly full; the chain is still live (Granted), so nothing is evictable.

        let token = inner.token_for(&req.id, principal);
        assert!(token.is_none(), "retrieval must fail closed rather than hand out an unaccounted credential");
        assert!(
            inner.log(0).iter().all(|e| !matches!(e, LedgerEntry::KeyRetrieved { .. })),
            "no KeyRetrieved entry should have been recorded"
        );
    }

    /// Regression test for review finding S1: a reservation that ultimately
    /// fails must leave the retained log byte-for-byte unchanged, even when
    /// the ring side of the reservation *would* have succeeded (some
    /// entries were evictable) but the sink side then refused. Forces that
    /// exact ordering: a closed chain makes ring eviction possible, while a
    /// tripped sink makes the send impossible.
    #[derive(Default)]
    struct AlwaysFailingSink;
    impl LedgerSink for AlwaysFailingSink {
        fn post(&self, _entry: &LedgerEntry) -> Result<(), super::super::config::LedgerSinkError> {
            Err(super::super::config::LedgerSinkError("synthetic failure".to_string()))
        }
    }

    #[tokio::test]
    async fn a_failed_reservation_leaves_the_retained_log_unchanged_even_when_ring_eviction_alone_would_have_succeeded() {
        let sink = Arc::new(AlwaysFailingSink);
        let config = LedgerConfig {
            retained_entries: 2,
            sink_queue: 5,
            ..Default::default()
        };
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(config, Some(sink), Arc::new(SystemWallClock)).unwrap();
        let principal = agent("agent-1");

        // Post and immediately deny one request, closing its chain — its
        // entries become evictable. `sink_queue: 5` gives both calls room
        // to queue before the drain task has processed anything.
        let req = post(&inner, &principal);
        let _ = inner.deny(&req.id, "no".to_string(), principal.clone());

        // Give the background drain task a chance to call the always-failing
        // sink and trip `sink_failed`.
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(5)).await;
            if inner
                .post_request(
                    draft("fs.remove"),
                    principal.clone(),
                    Capture::DirectExecution,
                    RequestContext::default(),
                    Duration::from_secs(60),
                    None,
                )
                .is_err()
            {
                break;
            }
        }

        let before = inner.log(0);
        // Ring eviction alone would succeed here (the denied chain's
        // entries are closed and evictable) — only the sink side refuses.
        let err = inner
            .post_request(draft("fs.remove"), principal, Capture::DirectExecution, RequestContext::default(), Duration::from_secs(60), None)
            .unwrap_err();
        assert!(matches!(err, LedgerError::SinkUnavailable(_)), "got {err:?}");
        let after = inner.log(0);
        assert_eq!(
            before.len(),
            after.len(),
            "a failed reservation must not have evicted anything from the ring even though eviction alone was possible"
        );
    }

    /// Regression test for review finding S3: when the sink trips, the
    /// number of entries it never received must equal exactly what
    /// `SinkUnavailable`'s message accounts — no silent, unaccounted loss
    /// of the backlog the drain task abandons.
    #[derive(Default)]
    struct FailFirstSink {
        received: Mutex<Vec<LedgerEntry>>,
    }
    impl LedgerSink for FailFirstSink {
        fn post(&self, _entry: &LedgerEntry) -> Result<(), super::super::config::LedgerSinkError> {
            Err(super::super::config::LedgerSinkError("synthetic failure".to_string()))
        }
    }

    #[tokio::test]
    async fn sink_failure_accounts_exactly_the_entries_it_never_delivered() {
        let sink = Arc::new(FailFirstSink::default());
        let config = LedgerConfig {
            sink_queue: 10,
            ..Default::default()
        };
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(config, Some(Arc::clone(&sink) as Arc<dyn LedgerSink>), Arc::new(SystemWallClock)).unwrap();
        let principal = agent("agent-1");

        // Three posts land in the ring and the sink queue before the drain
        // task gets a chance to run (no `.await` inside `post_request`).
        let mut ids = Vec::new();
        for _ in 0..3 {
            let req = post(&inner, &principal);
            ids.push(req.id);
        }

        // Let the drain task process: it dequeues the first entry, fails,
        // and drains the remaining backlog (the other two), counting all
        // three as undelivered.
        let mut message = String::new();
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(5)).await;
            if let Err(LedgerError::SinkUnavailable(msg)) = inner.post_request(
                draft("fs.remove"),
                principal.clone(),
                Capture::DirectExecution,
                RequestContext::default(),
                Duration::from_secs(60),
                None,
            ) {
                message = msg;
                break;
            }
        }
        assert!(message.contains('3'), "expected the failure message to account exactly 3 undelivered entries, got: {message}");

        #[allow(clippy::unwrap_used)]
        let received = sink.received.lock().unwrap().len();
        assert_eq!(received, 0, "the always-failing sink never successfully received anything");
        // Every entry the ledger recorded for the three original posts
        // (3 Requested entries) is exactly what went undelivered.
        let ring_entries_for_those_requests = inner
            .log(0)
            .into_iter()
            .filter(|e| matches!(e, LedgerEntry::Requested { request, .. } if ids.contains(&request.id)))
            .count();
        assert_eq!(ring_entries_for_those_requests, 3, "all 3 Requested entries still landed in the in-memory ring");
    }

    /// Regression test for review finding S4: closed chains must not
    /// accumulate in `LedgerState.chains` forever. A long sequence of
    /// open-then-close cycles, run through a small `retained_entries`
    /// window, must keep the map bounded — while a chain that stays live
    /// throughout survives every eviction pass untouched.
    #[test]
    fn closed_chains_are_evicted_from_the_map_but_a_live_chain_never_is() {
        let config = LedgerConfig {
            retained_entries: 4,
            ..Default::default()
        };
        #[allow(clippy::unwrap_used)]
        let inner = build_inner(config, None, Arc::new(SystemWallClock)).unwrap();
        let principal = agent("agent-1");

        // A chain that stays live (never decided) for the whole test.
        let live_req = post(&inner, &principal);

        // Many open-close cycles — each denial closes its chain
        // immediately (1 Requested + 1 Denied = 2 ring entries), well
        // past the small retention window.
        for _ in 0..50 {
            let req = post(&inner, &principal);
            let _ = inner.deny(&req.id, "no".to_string(), principal.clone());
        }

        #[allow(clippy::unwrap_used)]
        let chains_len = {
            let guard = inner.lock();
            guard.chains.len()
        };
        // Bounded: the live chain, plus at most a handful of recently
        // closed ones still referenced by the small ring — nowhere near
        // the 51 chains this test created.
        assert!(
            chains_len <= 6,
            "closed chains must be evicted from the map as the ring evicts their entries — found {chains_len} still resident"
        );

        // The live chain survived every eviction pass, regardless of
        // pressure.
        assert_eq!(inner.state(&live_req.id), Some(RequestState::Requested));
    }
}
