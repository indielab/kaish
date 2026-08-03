//! Subscription matching (`docs/approval-ledger.md` §C.5).
//!
//! A subscription is a glob over (operation, resource) that puts matching
//! filesystem operations into one of two postures: `observe` records them and
//! lets them run, `enforce` sends them through the real decision chain. It is
//! the generalization of the whole-`fs.*` enforce policy `set -o approvals`
//! installs.
//!
//! **The dominant constraint is that nothing subscribed costs nothing.** A
//! `rm -rf` over 10,000 paths must not pay a per-path ledger cost unless an
//! operator asked for one, so the filter is built in two steps: one relaxed
//! atomic load answering "is anything subscribed at all?" — almost always no,
//! branch predicted, done — and only then the glob work. Nothing is allocated
//! on the unsubscribed path, and `ApprovalRequest::constructed_count` is what
//! proves it.
//!
//! Two rules this module fixes, neither of which the spec states outright:
//!
//! - **`enforce` beats `observe` when both cover a path.** Enforce is the
//!   strictly stronger posture and produces a superset of observe's record,
//!   so the reverse precedence could silently downgrade a gate to a note.
//! - **A subscription matches per resource, not all-or-nothing.** A standing
//!   grant is all-or-nothing because it *authorizes*; a subscription only
//!   scopes, and `rm /workspace/a /tmp/b` under an `observe` subscription on
//!   `/workspace/**` must record `/workspace/a` and stay silent about
//!   `/tmp/b` (spec §C.5's own worked example).

use kaish_types::approval::{
    ApprovalRequest, OperationId, Subscription, SubscriptionId, SubscriptionMode,
};

use super::patterns::{covers_operation, covers_resource, covers_resource_parts};

/// What the §C.5 registry says about one operation on one resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posture {
    /// Nothing covers it. No request exists, nothing is allocated, and the
    /// ledger stays empty — spec §C.5's "unsubscribed and ungated means
    /// unposted".
    Unsubscribed,
    /// An `observe` subscription covers it: post the record, auto-grant it,
    /// and proceed. Never defers, never blocks, never returns exit 2.
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
/// Taken once rather than queried per path so a 10,000-path `rm` under a
/// live subscription acquires the ledger lock once, not 10,000 times. The
/// snapshot can go stale within a single command — a subscription revoked
/// mid-`rm` still covers the paths that command already classified — which is
/// the same "takes effect for operations not yet posted" rule revocation has
/// everywhere else (§C.4).
#[derive(Debug, Clone, Default)]
pub struct SubscriptionFilter {
    /// The whole-`fs.*` enforce policy (`set -o approvals`). Kept separate
    /// from the registry because a session flag is not a subscription: it is
    /// scoped to the session, not recorded as a ledger entry, and script can
    /// turn it on.
    policy: bool,
    /// The live registry, empty whenever the atomic said nothing was
    /// subscribed — which is what keeps the unsubscribed path allocation-free.
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
    /// enforce subscription over everything, so it answers first and cheapest.
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
                // deterministic precedence standing grants use (§C.4), rather
                // than a specificity metric nobody has defined.
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

/// Whether an `observe` subscription covers **every** resource on `request`,
/// and which one.
///
/// All-or-nothing here, unlike [`SubscriptionFilter::posture`], because this
/// is the auto-grant decision rather than the scoping filter: a grant
/// authorizes the request's whole resource set or it authorizes nothing
/// (§C.4). The gate site is what makes that reachable — it partitions paths
/// by posture and posts the observe-covered ones as their own request.
///
/// Pure glob work with no I/O, so the ledger's critical section may call it
/// under the lock (§B.1).
pub(crate) fn observing(
    subscriptions: &[Subscription],
    request: &ApprovalRequest,
) -> Option<SubscriptionId> {
    subscriptions
        .iter()
        .filter(|subscription| subscription.mode == SubscriptionMode::Observe)
        .filter(|subscription| covers_operation(&subscription.operations, &request.operation))
        .find(|subscription| {
            request
                .resources
                .iter()
                .all(|resource| covers_resource(&subscription.resources, resource))
        })
        .map(|subscription| subscription.id)
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
