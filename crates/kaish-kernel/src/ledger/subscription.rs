//! Subscription matching.
//!
//! A subscription is a glob over (operation, resource) that puts matching
//! filesystem operations into one of two postures: `observe` posts one
//! `Observed` entry and lets them run, `enforce` sends them through the
//! decision chain. It is the generalization of the whole-`fs.*` enforce
//! policy `set -o approvals` installs.
//!
//! **The filter's classification is the whole decision.** An observed path
//! never builds a request and never re-enters the ledger's matching — the
//! gate site records exactly what it classified, tagged with the winning
//! subscription's id. There is no second matcher to disagree with this one.
//!
//! **An unsubscribed session pays nothing.** A `rm -rf` over 10,000 paths
//! must not pay a per-path ledger cost unless an operator asked for one, so
//! the filter answers in two steps: one relaxed atomic load for "is anything
//! subscribed at all?" — almost always no — and only then the glob work.
//! Nothing is allocated when nothing is subscribed, and
//! `ApprovalRequest::constructed_count` is what proves it: 10,000 paths, 0
//! requests built.
//!
//! Two precedence rules:
//!
//! - **`enforce` beats `observe` when both cover a path.** Enforce is the
//!   stronger posture and its record is a superset of observe's, so the
//!   reverse order would turn a gate into a bare record.
//! - **A subscription matches per resource, not all-or-nothing.** A standing
//!   grant is all-or-nothing because it authorizes; a subscription only
//!   scopes. `rm /workspace/a /tmp/b` under an `observe` subscription on
//!   `/workspace/**` records `/workspace/a` and stays silent about `/tmp/b`.

use kaish_types::approval::{OperationId, Subscription, SubscriptionId, SubscriptionMode};

use super::patterns::{covers_operation, covers_resource_parts};

/// What the registry says about one operation on one resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posture {
    /// Nothing covers it. No request is built, nothing is allocated, and no
    /// ledger entry is posted.
    Unsubscribed,
    /// An `observe` subscription covers it: post one `Observed` entry and
    /// proceed. Never defers, never blocks, never returns exit 2 — no
    /// request is built and nothing is decided.
    Observe(SubscriptionId),
    /// The `set -o approvals` policy or an `enforce` subscription covers it:
    /// run the real decision chain.
    Enforce,
}

impl Posture {
    /// Whether this posture sends the operation through the decision chain.
    pub fn enforces(self) -> bool {
        matches!(self, Self::Enforce)
    }
}

/// A snapshot of the registry, taken once per gate call and matched per
/// path.
///
/// One snapshot rather than a query per path, so a 10,000-path `rm` under a
/// live subscription takes the ledger lock once, not 10,000 times. The
/// snapshot can go stale within a single command: a subscription revoked
/// mid-`rm` still covers the paths that command already classified. That is
/// the same rule every revocation here follows — it takes effect for
/// operations not yet posted.
#[derive(Debug, Clone, Default)]
pub struct SubscriptionFilter {
    /// The whole-`fs.*` enforce policy (`set -o approvals`). Kept separate
    /// from the registry because a session flag is not a subscription: it is
    /// scoped to the session, script can turn it on, and it posts no ledger
    /// entry of its own.
    policy: bool,
    /// The live registry. Empty whenever nothing is subscribed, which is
    /// what keeps that case free of allocation.
    subscriptions: Vec<Subscription>,
}

impl SubscriptionFilter {
    /// Build a filter from the enforce policy and a registry snapshot.
    pub fn new(policy: bool, subscriptions: Vec<Subscription>) -> Self {
        Self {
            policy,
            subscriptions,
        }
    }

    /// Whether anything at all could gate or record. `false` is the gate
    /// site's early-out: no request is built, no path is resolved for the
    /// ledger's benefit, and no entry is posted.
    pub fn engaged(&self) -> bool {
        self.policy || !self.subscriptions.is_empty()
    }

    /// The posture for one `path` resource under `operation`.
    ///
    /// `enforce` wins over `observe`, and the whole-namespace policy is an
    /// enforce subscription over everything, so it answers first and without
    /// touching the registry.
    pub fn posture(&self, operation: &OperationId, kind: &str, id: &str) -> Posture {
        if self.policy {
            return Posture::Enforce;
        }
        let mut observed: Option<SubscriptionId> = None;
        for subscription in &self.subscriptions {
            if !covers_operation(&subscription.operations, operation)
                || !covers_resource_parts(&subscription.resources, kind, id)
            {
                continue;
            }
            match subscription.mode {
                // Short-circuit: nothing weaker can overturn an enforce.
                SubscriptionMode::Enforce => return Posture::Enforce,
                // Lowest id wins among observers — issue order, the same
                // deterministic precedence standing grants use, rather than
                // a specificity metric nobody has defined.
                SubscriptionMode::Observe => {
                    observed.get_or_insert(subscription.id);
                }
                // `SubscriptionMode` is `#[non_exhaustive]`: a mode added
                // upstream with no case here must not silently mean "record
                // it and move on".
                _ => return Posture::Enforce,
            }
        }
        match observed {
            Some(id) => Posture::Observe(id),
            None => Posture::Unsubscribed,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use kaish_types::approval::{OperationPattern, ResourcePattern};

    fn subscription(
        id: u64,
        operations: &[&str],
        resources: &[(&str, &str)],
        mode: SubscriptionMode,
    ) -> Subscription {
        let mut s = Subscription::new(
            operations.iter().map(|o| OperationPattern::new(*o)).collect(),
            resources
                .iter()
                .map(|(kind, pattern)| ResourcePattern::new(*kind, *pattern))
                .collect(),
            mode,
            "test subscription",
        );
        s.id = SubscriptionId::new(id);
        s
    }

    fn operation(id: &str) -> OperationId {
        OperationId::new(id).unwrap()
    }

    #[test]
    fn an_empty_filter_is_not_engaged_and_postures_nothing() {
        let filter = SubscriptionFilter::default();
        assert!(!filter.engaged());
        assert_eq!(
            filter.posture(&operation("fs.remove"), "path", "/workspace/a"),
            Posture::Unsubscribed
        );
    }

    #[test]
    fn the_enforce_policy_covers_every_path_without_consulting_the_registry() {
        let filter = SubscriptionFilter::new(true, Vec::new());
        assert!(filter.engaged());
        for path in ["/workspace/a", "/tmp/b", "/"] {
            assert_eq!(
                filter.posture(&operation("fs.remove"), "path", path),
                Posture::Enforce
            );
        }
    }

    #[test]
    fn an_observe_subscription_scopes_by_glob_and_leaves_the_rest_free() {
        let filter = SubscriptionFilter::new(
            false,
            vec![subscription(
                7,
                &["fs.remove", "fs.overwrite"],
                &[("path", "/workspace/**")],
                SubscriptionMode::Observe,
            )],
        );
        assert!(filter.engaged());
        assert_eq!(
            filter.posture(&operation("fs.remove"), "path", "/workspace/deep/a"),
            Posture::Observe(SubscriptionId::new(7))
        );
        // Outside the glob.
        assert_eq!(
            filter.posture(&operation("fs.remove"), "path", "/tmp/b"),
            Posture::Unsubscribed
        );
        // Inside the glob, operation not named.
        assert_eq!(
            filter.posture(&operation("fs.rename"), "path", "/workspace/a"),
            Posture::Unsubscribed
        );
        // Inside the glob, wrong resource kind — kind never globs.
        assert_eq!(
            filter.posture(&operation("fs.remove"), "git.ref", "/workspace/a"),
            Posture::Unsubscribed
        );
    }

    #[test]
    fn enforce_beats_observe_whichever_order_they_are_registered_in() {
        let observe = subscription(
            1,
            &["fs.*"],
            &[("path", "/workspace/**")],
            SubscriptionMode::Observe,
        );
        let enforce = subscription(
            2,
            &["fs.*"],
            &[("path", "/workspace/secret/**")],
            SubscriptionMode::Enforce,
        );
        for subs in [
            vec![observe.clone(), enforce.clone()],
            vec![enforce, observe],
        ] {
            let filter = SubscriptionFilter::new(false, subs);
            assert_eq!(
                filter.posture(&operation("fs.remove"), "path", "/workspace/secret/k"),
                Posture::Enforce
            );
            assert_eq!(
                filter.posture(&operation("fs.remove"), "path", "/workspace/open/k"),
                Posture::Observe(SubscriptionId::new(1))
            );
        }
    }

    #[test]
    fn a_subscription_naming_no_operation_or_no_resource_matches_nothing() {
        let no_operation = SubscriptionFilter::new(
            false,
            vec![subscription(
                1,
                &[],
                &[("path", "/**")],
                SubscriptionMode::Observe,
            )],
        );
        assert_eq!(
            no_operation.posture(&operation("fs.remove"), "path", "/a"),
            Posture::Unsubscribed
        );

        let no_resource = SubscriptionFilter::new(
            false,
            vec![subscription(1, &["fs.*"], &[], SubscriptionMode::Observe)],
        );
        assert_eq!(
            no_resource.posture(&operation("fs.remove"), "path", "/a"),
            Posture::Unsubscribed
        );
    }
}
