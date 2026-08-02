//! Ledger sizing, the audit-sink trait, and the sink's own error type
//! (`docs/approval-ledger.md` §D.4).

use std::time::Duration;

use kaish_types::approval::LedgerEntry;

/// Sizing and timeouts for one [`super::Ledger`] (spec §D.4). Every field has
/// a default matching the spec's stated number; construct with
/// `LedgerConfig { field: ..., ..Default::default() }` to override one —
/// deliberately **not** `#[non_exhaustive]`, unlike the rest of this
/// module's public types, so that functional-update pattern keeps working
/// for embedders (and this crate's own integration tests) as fields are
/// added.
#[derive(Debug, Clone)]
pub struct LedgerConfig {
    /// Maximum LIVE (unclosed) requests. Closed chains do not count against
    /// it. Default 1024.
    pub live_capacity: usize,
    /// Per-principal share of `live_capacity` — one principal cannot starve
    /// the others. Default 256.
    pub live_capacity_per_principal: usize,
    /// Retained entries in the audit ring, oldest evicted first — but never
    /// an entry belonging to a still-live request (spec §D.4). Default 4096.
    pub retained_entries: usize,
    /// Bounded sink queue depth. Default 1024 entries.
    pub sink_queue: usize,
    /// How long a request stays live with no decision. Default 60s (today's
    /// nonce TTL).
    pub request_ttl: Duration,
    /// The rejection count at which a request's chain voids. Default 5 (the
    /// counter kaish #259 deferred — spec §F.3).
    pub max_token_attempts: u32,
    /// How long a `Reserved` attempt may go unreported before the recovery
    /// sweep closes it as `Abandoned`. Not in the spec's own `LedgerConfig`
    /// field list — added here because PR 3's `AttemptGuard` (the drop-safe
    /// settlement guard that gives the sweep a real "the process died"
    /// signal) does not land until the next PR, and a sweep with no
    /// staleness bound at all would never resolve to
    /// `Outcome::Unknown{ExecutorLost}`. Default 10 minutes — generous
    /// enough that no in-flight operation should ever cross it. Superseded
    /// once `AttemptGuard`'s outbox exists; flagged for review then.
    pub attempt_stale_after: Duration,
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            live_capacity: 1024,
            live_capacity_per_principal: 256,
            retained_entries: 4096,
            sink_queue: 1024,
            request_ttl: Duration::from_secs(60),
            max_token_attempts: 5,
            attempt_stale_after: Duration::from_secs(600),
        }
    }
}

/// Why a [`LedgerSink::post`] call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerSinkError(pub String);

impl std::fmt::Display for LedgerSinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ledger sink: {}", self.0)
    }
}

impl std::error::Error for LedgerSinkError {}

/// An export destination for the ledger's append-only log (spec §D.4).
///
/// `post` must be fast and non-blocking — the ledger calls it from a
/// background task that drains a bounded queue (`LedgerConfig::sink_queue`)
/// so a slow sink applies backpressure (`LedgerError::SinkUnavailable` at the
/// next obligation post) rather than blocking the async executor. A sink that
/// fronts something slow (a network log) should buffer internally and return
/// `Ok`, accepting the buffering risk explicitly — the kernel will not make
/// that call silently.
///
/// An `Err` here is treated as the sink itself being broken: the ledger
/// stops accepting new obligations (fails closed) rather than continuing to
/// produce audit entries it cannot deliver. There is no retry — a tripped
/// sink stays tripped for the life of the ledger.
pub trait LedgerSink: Send + Sync {
    /// Append one entry. See the trait doc for the non-blocking contract and
    /// what an `Err` does to the ledger.
    fn post(&self, entry: &LedgerEntry) -> Result<(), LedgerSinkError>;
}
