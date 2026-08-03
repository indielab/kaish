//! The two pattern primitives standing grants (§C.4) and subscriptions
//! (§C.5) both match with, kept in one place so their glob semantics cannot
//! drift apart.
//!
//! Both are pure and lock-free: CPU-only glob work with no I/O and no
//! `.await`, which is the only reason the ledger's one critical section is
//! allowed to call them while it holds the lock (spec §B.1).
//!
//! - **Operations glob**, so `fs.*` covers a namespace.
//! - **Resource kinds match exactly; only the `id` globs** (spec §C.4). A
//!   `*` in the kind position is a literal kind named `*`, not a wildcard —
//!   a typed resource must never be matched by a pattern that never named
//!   its type.
//!
//! An empty pattern list is **inert, not universal**, in both positions: an
//! empty list is far more likely to be a construction mistake than a
//! deliberate "everything", and the safe reading of a mistake is "match
//! nothing".

use kaish_glob::glob_match;
use kaish_types::approval::{OperationId, OperationPattern, Resource, ResourcePattern};

/// Whether any of `patterns` globs `operation`.
pub(crate) fn covers_operation(patterns: &[OperationPattern], operation: &OperationId) -> bool {
    patterns
        .iter()
        .any(|pattern| glob_match(pattern.as_str(), operation.as_str()))
}

/// Whether any of `patterns` covers `resource`: kind equal, id glob.
pub(crate) fn covers_resource(patterns: &[ResourcePattern], resource: &Resource) -> bool {
    patterns
        .iter()
        .any(|pattern| pattern.kind == resource.kind && glob_match(&pattern.pattern, &resource.id))
}

/// Whether any of `patterns` covers a resource named by its parts, without
/// building a [`Resource`]. This is the §C.5 gate-site filter's entry point:
/// it runs once per path on an operation an operator subscribed to, and the
/// whole point of the subscription design is that a path decides its own
/// posture before anything is allocated for it.
pub(crate) fn covers_resource_parts(patterns: &[ResourcePattern], kind: &str, id: &str) -> bool {
    patterns
        .iter()
        .any(|pattern| pattern.kind == kind && glob_match(&pattern.pattern, id))
}
