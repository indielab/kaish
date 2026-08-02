//! The approval ledger core (`docs/approval-ledger.md`, ledger PR 2).
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
//! chain (§D.4), sink backpressure, the invariant checks, and the recovery
//! sweep.
//!
//! **What is not:** this module is wired to no gate site. Nothing in
//! `kaish-kernel` calls into it yet, and building one changes no observable
//! behavior anywhere else in the crate. `ToolCtx::request_approval` and the
//! drop-safe `AttemptGuard` (PR 3), the `Approver` decision chain and
//! standing-grant *matching* (PR 4), `Kernel::confirm` and the ten gate
//! sites (PR 5, the cutover) all build on this module without changing it.

mod config;
mod core;
mod error;
mod handles;

pub use config::{LedgerConfig, LedgerSink, LedgerSinkError};
pub use error::LedgerError;
pub use handles::{ApproverHandle, AttemptHandle, AttemptView, Approvals, Ledger, RequestChain, Requester};
