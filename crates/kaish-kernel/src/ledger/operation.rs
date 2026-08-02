//! The kernel's operation taxonomy (`docs/approval-ledger.md` §A.6).
//!
//! In-tree operations come from a closed enum and the mapping to the dotted
//! id is an exhaustive match, so **adding a gate site without registering its
//! operation is a compile error**. Plugins get
//! [`OperationId::namespaced`](kaish_types::approval::OperationId::namespaced),
//! which refuses the reserved `fs.`/`trash.` prefixes — the `fs.` namespace
//! belongs to the kernel, and a policy engine's vocabulary stays honest
//! because of it.

use kaish_types::approval::{OperationId, RiskClass};

/// Every operation a kernel gate site can post. Closed by design: the
/// `id`/`risk` matches below are exhaustive, so a new gate site must name
/// itself here before it can request anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelOperation {
    /// `rm` removing a path permanently — the trash did not catch it.
    FsRemove,
    /// A truncating overwrite of an existing file (`cp`, `dd`, `patch`,
    /// `sed -i`, `tee`, `write`).
    FsOverwrite,
    /// `mv` replacing an existing destination.
    FsRename,
    /// `kaish-trash empty` discarding the recovery net itself.
    TrashEmpty,
}

impl KernelOperation {
    /// The dotted id this operation posts under. Exhaustive by construction.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FsRemove => "fs.remove",
            Self::FsOverwrite => "fs.overwrite",
            Self::FsRename => "fs.rename",
            Self::TrashEmpty => "trash.empty",
        }
    }

    /// How hard this operation is to walk back. An approver reads it and a
    /// policy matches on it; it carries no redemption policy of its own
    /// (spec §F.3 item 4 — every grant authorizes exactly one successful
    /// settlement, in every risk class alike).
    pub const fn risk(self) -> RiskClass {
        match self {
            // The trash already caught everything it could — what reaches
            // these gates is the case with no recovery net left.
            Self::FsRemove | Self::FsOverwrite | Self::FsRename | Self::TrashEmpty => {
                RiskClass::Irreversible
            }
        }
    }

    /// Whether this operation gates regardless of any subscription or policy
    /// (spec §F.1). Only `trash.empty` does: it discards the recovery net
    /// that makes every other `fs.*` operation survivable, so it is not
    /// something a session can turn off.
    pub const fn always_enforced(self) -> bool {
        matches!(self, Self::TrashEmpty)
    }

    /// The typed id, ready to draft against.
    pub fn id(self) -> OperationId {
        // Infallible: every string above is a well-formed dotted id, and this
        // is proven by `every_operation_builds_a_valid_id` below.
        OperationId::new(self.as_str()).unwrap_or_else(|e| {
            unreachable!("kernel operation {:?} is not a valid OperationId: {e}", self)
        })
    }
}

impl std::fmt::Display for KernelOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const ALL: &[KernelOperation] = &[
        KernelOperation::FsRemove,
        KernelOperation::FsOverwrite,
        KernelOperation::FsRename,
        KernelOperation::TrashEmpty,
    ];

    #[test]
    fn every_operation_builds_a_valid_id() {
        // `id()` claims infallibility. Prove it for every variant rather than
        // discovering a malformed id at a gate site under load.
        for op in ALL {
            assert_eq!(op.id().as_str(), op.as_str());
        }
    }

    #[test]
    fn every_operation_sits_in_a_kernel_reserved_namespace() {
        // The other half of §A.6: a plugin cannot post any of these, because
        // `OperationId::namespaced` refuses the prefixes they use.
        for op in ALL {
            let prefix = op.as_str().split('.').next().unwrap();
            assert!(
                OperationId::namespaced(prefix, "anything").is_err(),
                "{op} sits in a namespace a plugin can still claim"
            );
        }
    }

    #[test]
    fn only_trash_empty_is_always_enforced() {
        for op in ALL {
            assert_eq!(
                op.always_enforced(),
                *op == KernelOperation::TrashEmpty,
                "{op} disagrees with the always-enforced rule"
            );
        }
    }

    #[test]
    fn ids_are_distinct() {
        let mut ids: Vec<&str> = ALL.iter().map(|o| o.as_str()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "two operations share one dotted id");
    }
}
