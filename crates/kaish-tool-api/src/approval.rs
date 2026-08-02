//! `ToolCtx`'s approval-ledger surface (`docs/approval-ledger.md`, ledger PR
//! 3): the types a tool sees when it gates a privileged operation through
//! [`crate::ToolCtx::request_approval`].
//!
//! These are deliberately **not** `kaish_kernel::ledger`'s handle types —
//! this crate cannot depend on `kaish-kernel` (the dependency runs the other
//! way: `kaish-kernel`'s `ExecContext` implements `ToolCtx`). `AttemptHandle`
//! and `Approvals` here are small, tool-facing values that the kernel's real
//! `ToolCtx` impl builds from its own live ledger state; `kaish-kernel`'s
//! `ledger::AttemptHandle`/`ledger::Approvals` remain the embedder-level
//! handles behind `Kernel::approvals()` (spec §D.2) and are unrelated types
//! that happen to share a name with their tool-facing counterpart here.

use kaish_types::approval::{AttemptId, RequestId};
use kaish_types::ExecResult;

/// What one execution reserved against a grant (spec §C.1). Exposes only its
/// own two ids — never provenance: a tool cannot tell whether its grant came
/// from a human, a policy hook, or a standing rule.
///
/// No public constructor outside `#[cfg(test)]` — see
/// `AttemptHandle::from_ids`'s doc for why a `pub` one (even
/// `#[doc(hidden)]`) would be a forgeable settlement capability. A doctest
/// compiles with none of this crate's own `#[cfg(test)]` items visible (the
/// same boundary a downstream crate sees), so this is a real compiled proof,
/// not a comment:
///
/// ```compile_fail
/// use kaish_tool_api::AttemptHandle;
/// use kaish_types::approval::{AttemptId, RequestId};
///
/// // `from_ids` doesn't exist here — it's `#[cfg(test)] pub(crate)` in the
/// // defining crate, invisible to a doctest (compiled as downstream code)
/// // exactly as it would be to a real plugin crate.
/// let _handle = AttemptHandle::from_ids(RequestId::parse("req_00000000_1").unwrap(), AttemptId::new(1));
/// ```
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

    /// Test-only: build a handle from raw ids without a real reservation.
    ///
    /// Deliberately **not** exposed outside `#[cfg(test)]` — unlike
    /// [`ToolCtx::as_any_mut`](crate::ToolCtx::as_any_mut), this is not a
    /// documented escape hatch for trusted callers. `settle`'s job is to
    /// finalize whatever `(request, attempt)` pair names a live `Reserved`
    /// attempt; unlike `redeem`, it does not — and structurally cannot —
    /// verify that the caller is the one who reserved it, so a `pub`
    /// constructor here (even `#[doc(hidden)]`) would let any code holding a
    /// `&mut dyn ToolCtx` settle *any* live attempt in the ledger by
    /// guessing or observing its ids, not just its own. `ApprovalOutcome`'s
    /// only real producer is the kernel's own `ExecContext`, which never
    /// needs this constructor: PR 3 wires no decision chain, so nothing
    /// today ever returns `Authorized`. When PR 4's decision chain starts
    /// returning `Authorized` for real, minting the handle needs an
    /// unforgeable capability (e.g. a per-reservation token bound to the
    /// execution that redeemed it), which is that PR's job — not a
    /// cross-crate id constructor.
    #[cfg(test)]
    pub(crate) fn from_ids(request: RequestId, attempt: AttemptId) -> Self {
        Self { request, attempt }
    }
}

/// The decision `ToolCtx::request_approval` returns (spec §C.1). Every
/// variant but `Authorized` fails closed — see [`Self::proceed`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ApprovalOutcome {
    /// A grant existed (or was decided inline) and an attempt is reserved.
    Authorized(AttemptHandle),
    /// No decision yet. The view is tokenless (spec §A.2).
    Pending(Box<kaish_types::approval::ApprovalRequestView>),
    /// The request was denied.
    Denied {
        /// The denied request.
        request: RequestId,
        /// Why.
        reason: String,
    },
    /// A precondition on the grant no longer holds, or could not be
    /// observed. The grant is voided and the operation must re-request
    /// (spec §B.4).
    Refused {
        /// The refused request.
        request: RequestId,
        /// Why.
        detail: String,
    },
    /// This context has no ledger — a unit-test harness or a minimal
    /// embedder. The default [`crate::ToolCtx::request_approval`] impl
    /// always returns this.
    Unsupported,
    /// The ledger refused to record: sink backpressure or live capacity
    /// (spec §D.4).
    LedgerUnavailable {
        /// Why.
        reason: String,
    },
}

impl ApprovalOutcome {
    /// Convert every non-`Authorized` variant into the `ExecResult` a gate
    /// site returns verbatim (spec §C.1) — the one call pattern:
    ///
    /// ```ignore
    /// let attempt = ctx.request_approval(req).await.proceed()?;
    /// ```
    ///
    /// `Pending` maps to exit 2 with the view on [`ExecResult::approval`];
    /// `Denied`, `Refused`, `Unsupported`, and `LedgerUnavailable` map to
    /// exit 1 with a message naming the reason. `Authorized` is the only
    /// variant that lets the caller continue.
    // `ExecResult` is deliberately fat (see `kaish_trash.rs`'s identical
    // allow) — a gate site returns it verbatim, and boxing it here would
    // just move the same allocation the caller immediately unboxes again.
    #[allow(clippy::result_large_err)]
    pub fn proceed(self) -> Result<AttemptHandle, ExecResult> {
        match self {
            Self::Authorized(attempt) => Ok(attempt),
            Self::Pending(view) => {
                let mut result = ExecResult::failure(
                    2,
                    format!("pending approval {} — an operator must grant it", view.id),
                );
                result.approval = Some(view);
                Err(result)
            }
            Self::Denied { request, reason } => {
                Err(ExecResult::failure(1, format!("request {request} denied: {reason}")))
            }
            Self::Refused { request, detail } => {
                Err(ExecResult::failure(1, format!("request {request} refused: {detail}")))
            }
            Self::Unsupported => Err(ExecResult::failure(
                1,
                "approval ledger not available in this context",
            )),
            Self::LedgerUnavailable { reason } => {
                Err(ExecResult::failure(1, format!("approval ledger unavailable: {reason}")))
            }
        }
    }
}

/// Read-only view for tools that surface pending approvals (`approvals`,
/// `wait`, `jobs` — spec §D.1). [`Self::empty`] backs the default
/// [`crate::ToolCtx::approvals`] for a context with no ledger and grants
/// nothing, because it cannot: it carries no authority, only a snapshot.
#[derive(Debug, Clone, Default)]
pub struct Approvals {
    pending: Vec<kaish_types::approval::ApprovalRequestView>,
}

impl Approvals {
    /// A view with nothing pending — the default for a context with no
    /// ledger.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Every currently-pending (undecided) request, as of when this
    /// snapshot was taken.
    pub fn pending(&self) -> &[kaish_types::approval::ApprovalRequestView] {
        &self.pending
    }

    /// Build a snapshot from a live pending list.
    ///
    /// Not part of the supported public surface — for the kernel's
    /// `ExecContext` to populate a real snapshot from its ledger handle.
    #[doc(hidden)]
    pub fn from_pending(pending: Vec<kaish_types::approval::ApprovalRequestView>) -> Self {
        Self { pending }
    }
}
