//! `fs.*` observability subscriptions (`docs/approval-ledger.md` §C.5, ledger
//! PR 8): the glob filter, `Grounds::Observe`, and the two modes.
//!
//! Everything drives real command strings through `kernel.execute()`, so the
//! full path runs — glob expansion, the gate site's per-path classification,
//! the decision chain, and the dispatch seam's settlement.
//!
//! The free-when-unsubscribed proof lives in `approval_zero_cost_tests.rs`,
//! alone in its own binary: its counter is process-wide, and these tests
//! deliberately build requests.

// Test-fixture code: unwrap/expect on known-good setup is the idiom here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

// KernelConfig::repl() mounts the real filesystem.
#![cfg(feature = "localfs")]

use std::path::Path;

use kaish_kernel::interpreter::ExecResult;
use kaish_kernel::ledger::ApproverHandle;
use kaish_kernel::{Kernel, KernelConfig};
use kaish_types::approval::{
    ApprovalRequest, Grounds, LedgerEntry, OperationPattern, ResourcePattern, Subscription,
    SubscriptionId, SubscriptionMode,
};

fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("approval-subscription-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("tempdir under CARGO_TARGET_TMPDIR")
}

/// A kernel, the authority its construction minted, and a two-directory tree:
/// `workspace/` (what the tests subscribe to) and `scratch/` (what they leave
/// unsubscribed, standing in for §C.5's `/tmp/**`).
struct Session {
    kernel: Kernel,
    authority: ApproverHandle,
    root: tempfile::TempDir,
}

impl Session {
    fn new() -> Self {
        let root = tempdir();
        std::fs::create_dir(root.path().join("workspace")).unwrap();
        std::fs::create_dir(root.path().join("scratch")).unwrap();
        let config = KernelConfig::repl()
            .with_cwd(root.path().to_path_buf())
            .with_approvals(false)
            .with_trash(false);
        let (kernel, authority) = Kernel::build(config).expect("kernel");
        Self {
            kernel,
            authority,
            root,
        }
    }

    fn workspace(&self, name: &str) -> std::path::PathBuf {
        self.root.path().join("workspace").join(name)
    }

    fn scratch(&self, name: &str) -> std::path::PathBuf {
        self.root.path().join("scratch").join(name)
    }

    fn write(&self, path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
    }

    async fn run(&self, script: &str) -> ExecResult {
        self.kernel.execute(script).await.expect("kernel execute")
    }

    /// Subscribe `mode` to `operations` over everything under `workspace/`.
    async fn subscribe_workspace(
        &self,
        operations: &[&str],
        mode: SubscriptionMode,
    ) -> SubscriptionId {
        let glob = format!("{}/**", self.root.path().join("workspace").display());
        self.authority
            .subscribe(Subscription::new(
                operations.iter().map(|o| OperationPattern::new(*o)).collect(),
                vec![ResourcePattern::new("path", glob)],
                mode,
                "the test subscribes to the workspace",
            ))
            .await
            .expect("the subscription must register")
    }

    /// Every retained entry's variant name, in commit order.
    fn entry_kinds(&self) -> Vec<&'static str> {
        self.kernel
            .approvals()
            .log(0)
            .iter()
            .map(entry_kind)
            .collect()
    }

    /// The grounds on every `Granted` entry, in commit order.
    fn grounds(&self) -> Vec<Grounds> {
        self.kernel
            .approvals()
            .log(0)
            .into_iter()
            .filter_map(|entry| match entry {
                LedgerEntry::Granted { grant, .. } => Some(grant.grounds),
                _ => None,
            })
            .collect()
    }

    /// Every `path` resource id named by every `Requested` entry, in commit
    /// order.
    fn requested_paths(&self) -> Vec<String> {
        self.kernel
            .approvals()
            .log(0)
            .into_iter()
            .filter_map(|entry| match entry {
                LedgerEntry::Requested { request, .. } => Some(request),
                _ => None,
            })
            .flat_map(|request| {
                request
                    .resources
                    .into_iter()
                    .map(|resource| resource.id)
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

fn entry_kind(entry: &LedgerEntry) -> &'static str {
    match entry {
        LedgerEntry::Requested { .. } => "Requested",
        LedgerEntry::Granted { .. } => "Granted",
        LedgerEntry::Denied { .. } => "Denied",
        LedgerEntry::Redeemed { .. } => "Redeemed",
        LedgerEntry::Settled { .. } => "Settled",
        LedgerEntry::KeyRetrieved { .. } => "KeyRetrieved",
        LedgerEntry::TokenRejected { .. } => "TokenRejected",
        LedgerEntry::Voided { .. } => "Voided",
        LedgerEntry::Expired { .. } => "Expired",
        LedgerEntry::Abandoned { .. } => "Abandoned",
        LedgerEntry::Refused { .. } => "Refused",
        LedgerEntry::StandingIssued { .. } => "StandingIssued",
        LedgerEntry::StandingRevoked { .. } => "StandingRevoked",
        LedgerEntry::Subscribed { .. } => "Subscribed",
        LedgerEntry::Unsubscribed { .. } => "Unsubscribed",
        _ => "other",
    }
}

/// §C.5's worked example: subscribe `fs.remove` under the workspace as
/// `observe`, and everything outside it stays unsubscribed and free.
#[tokio::test]
async fn an_observe_subscription_records_matching_paths_and_stays_silent_about_the_rest() {
    let session = Session::new();
    let inside = session.workspace("a.txt");
    let outside = session.scratch("b.txt");
    session.write(&inside, "inside");
    session.write(&outside, "outside");

    let id = session
        .subscribe_workspace(&["fs.remove"], SubscriptionMode::Observe)
        .await;

    let result = session
        .run(&format!("rm {} {}", inside.display(), outside.display()))
        .await;
    assert_eq!(result.code, 0, "{}", result.text_out());
    assert!(!inside.exists() && !outside.exists(), "both must be deleted");

    // One full chain for the covered path: posted, auto-granted by the
    // subscription, redeemed, and settled with the invocation's exit code.
    assert_eq!(
        session.entry_kinds(),
        vec![
            "Subscribed",
            "Requested",
            "Granted",
            "Redeemed",
            "Settled"
        ],
    );
    assert_eq!(
        session.grounds(),
        vec![Grounds::Observe { subscription: id }],
        "an observe subscription must name itself as the grounds"
    );
    // The unsubscribed path is absent from the record entirely — it was never
    // a resource on any request.
    assert_eq!(
        session.requested_paths(),
        vec![inside.display().to_string()],
        "only the covered path may reach the ledger"
    );
}

/// The same subscription over `fs.overwrite`, through the other gate site
/// (`gate_overwrites`) — so the filter is proven at both, not just at `rm`.
#[tokio::test]
async fn an_observe_subscription_records_an_overwrite_through_the_write_gate() {
    let session = Session::new();
    let inside = session.workspace("a.txt");
    let outside = session.scratch("b.txt");
    session.write(&inside, "before");
    session.write(&outside, "before");

    session
        .subscribe_workspace(&["fs.*"], SubscriptionMode::Observe)
        .await;

    let result = session
        .run(&format!(
            "echo after | tee {} {}",
            inside.display(),
            outside.display()
        ))
        .await;
    assert_eq!(result.code, 0, "{}", result.text_out());
    assert_eq!(std::fs::read_to_string(&inside).unwrap().trim(), "after");
    assert_eq!(std::fs::read_to_string(&outside).unwrap().trim(), "after");

    assert_eq!(
        session.requested_paths(),
        vec![inside.display().to_string()],
        "only the covered path may reach the ledger"
    );
}

/// The property that separates the two modes: `observe` is a note, not a
/// permission. It never defers, never returns exit 2, and never attaches a
/// pending request to the result.
#[tokio::test]
async fn an_observe_subscription_never_blocks_and_never_returns_exit_two() {
    let session = Session::new();
    let target = session.workspace("a.txt");
    session.write(&target, "content");

    session
        .subscribe_workspace(&["fs.*"], SubscriptionMode::Observe)
        .await;

    // The counter proves the request really was built — without it, "never
    // exit 2" would also pass on a filter that matched nothing at all.
    let before = ApprovalRequest::constructed_count();
    let result = session.run(&format!("rm {}", target.display())).await;
    assert!(
        ApprovalRequest::constructed_count() > before,
        "the observe path must actually build a request"
    );

    assert_eq!(result.code, 0, "{}", result.text_out());
    assert!(
        result.approval_request().is_none(),
        "an observe subscription must not surface a pending request"
    );
    assert!(!target.exists(), "the delete must have run");
    assert!(
        session.kernel.approvals().pending().is_empty(),
        "an observe subscription must leave nothing undecided"
    );
}

/// The other mode over the identical glob: `enforce` sends the same operation
/// through the real decision chain, so it holds at exit 2 with a pending
/// request.
#[tokio::test]
async fn an_enforce_subscription_over_the_same_glob_gates() {
    let session = Session::new();
    let inside = session.workspace("a.txt");
    let outside = session.scratch("b.txt");
    session.write(&inside, "inside");
    session.write(&outside, "outside");

    session
        .subscribe_workspace(&["fs.remove"], SubscriptionMode::Enforce)
        .await;

    let result = session
        .run(&format!("rm {} {}", inside.display(), outside.display()))
        .await;
    assert_eq!(result.code, 2, "{}", result.text_out());
    let view = result.approval_request().expect("a pending request");
    assert_eq!(view.operation.as_str(), "fs.remove");
    assert_eq!(
        view.resources
            .iter()
            .map(|r| r.id.clone())
            .collect::<Vec<_>>(),
        vec![inside.display().to_string()],
        "only the covered path may be gated"
    );

    // Nothing ran — a batch held at the gate holds every path in it, covered
    // or not, because the command returns before it deletes anything.
    assert!(inside.exists() && outside.exists());
    assert_eq!(session.entry_kinds(), vec!["Subscribed", "Requested"]);
}

/// Spec §C.5, and the reason it is worth stating: an audit scope that changed
/// with no record of the change makes the record it produced unreadable.
#[tokio::test]
async fn subscription_and_revocation_are_themselves_ledger_entries() {
    let session = Session::new();
    assert!(
        !session.kernel.approvals().any_subscriptions(),
        "a fresh ledger is subscribed to nothing"
    );

    let id = session
        .subscribe_workspace(&["fs.*"], SubscriptionMode::Observe)
        .await;
    assert!(session.kernel.approvals().any_subscriptions());
    assert_eq!(session.kernel.approvals().subscriptions().len(), 1);

    match session.kernel.approvals().log(0).as_slice() {
        [LedgerEntry::Subscribed { subscription, .. }] => {
            assert_eq!(subscription.id, id, "the entry carries the allocated id");
            assert_eq!(subscription.mode, SubscriptionMode::Observe);
            assert_eq!(subscription.reason, "the test subscribes to the workspace");
        }
        other => panic!("expected exactly one Subscribed entry, got {other:?}"),
    }

    session
        .authority
        .unsubscribe(&id, "the test is done watching")
        .await
        .expect("the revocation must post");

    assert!(
        !session.kernel.approvals().any_subscriptions(),
        "the fast path must disarm once the registry is empty"
    );
    assert!(session.kernel.approvals().subscriptions().is_empty());
    assert_eq!(session.entry_kinds(), vec!["Subscribed", "Unsubscribed"]);
    match session.kernel.approvals().log(0).last() {
        Some(LedgerEntry::Unsubscribed { id: revoked, reason, .. }) => {
            assert_eq!(*revoked, id);
            assert_eq!(reason, "the test is done watching");
        }
        other => panic!("expected an Unsubscribed entry, got {other:?}"),
    }

    // And it takes effect: a delete after the revocation posts nothing.
    let target = session.workspace("a.txt");
    session.write(&target, "content");
    let result = session.run(&format!("rm {}", target.display())).await;
    assert_eq!(result.code, 0, "{}", result.text_out());
    assert_eq!(
        session.entry_kinds(),
        vec!["Subscribed", "Unsubscribed"],
        "a revoked subscription must record nothing further"
    );
}

/// Revoking an id that was never issued is loud, not a no-op — a caller that
/// believes it turned off an audit scope must not be told it succeeded.
#[tokio::test]
async fn revoking_an_unknown_subscription_fails_loudly() {
    let session = Session::new();
    let err = session
        .authority
        .unsubscribe(&SubscriptionId::new(99), "never issued")
        .await
        .expect_err("an unknown id must not succeed");
    assert!(
        err.to_string().contains("subscription 99 does not exist"),
        "{err}"
    );
}
