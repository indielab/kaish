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
mod standing;

pub use approver::{
    Approver, ChainContext, ChainOutcome, ChainStage, DecisionChain, PatientSource,
    DEFAULT_DECIDE_BUDGET,
};
pub use attempt_guard::AttemptGuard;
pub use config::{LedgerConfig, LedgerSink, LedgerSinkError};
pub use error::LedgerError;
pub use handles::{ApproverHandle, AttemptHandle, AttemptView, Approvals, Ledger, RequestChain, Requester};
