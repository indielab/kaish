//! Redemption-time precondition verification (`docs/approval-ledger.md`
//! §B.4, ledger PR 6): a grant carries the state its request claimed, and
//! redemption re-observes that state before an attempt is reserved.
//!
//! The property under test is one sentence: **an approval granted against a
//! world that has since moved does not run.** Everything here drives real
//! command strings through `kernel.execute()` or `Kernel::confirm`, so the
//! resolver, the observation, the ledger transaction, and the builtin's own
//! write path all participate.
//!
//! The path resolver reads through the backend, so a tempdir under
//! `CARGO_TARGET_TMPDIR` is the fixture (never `/tmp`, which the trash gate
//! excludes and which would take a different decision branch).

// Test-fixture code: unwrap/expect on known-good setup is the idiom here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

// KernelConfig::repl() mounts the real filesystem.
#![cfg(feature = "localfs")]

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kaish_kernel::interpreter::ExecResult;
use kaish_kernel::ledger::{
    ApproverHandle, Ledger, LedgerConfig, ResolverError, StateResolver,
};
use kaish_kernel::tools::{ToolArgs, ToolCtx, ToolSchema};
use kaish_kernel::vfs::{MemoryFs, VfsRouter};
use kaish_kernel::{Kernel, KernelBackend, KernelConfig, LocalBackend, Tool};
use kaish_types::approval::{
    ApprovalRequest, GrantTerms, LedgerEntry, RequestId, RequestState, Resource, StateClaim,
};

fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("approval-precondition-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("tempdir under CARGO_TARGET_TMPDIR")
}

/// A kernel and the authority its construction minted.
struct Session {
    kernel: Kernel,
    authority: ApproverHandle,
}

impl Session {
    async fn run(&self, script: &str) -> ExecResult {
        self.kernel.execute(script).await.expect("kernel execute")
    }

    /// Grant the one pending request on the terms it declared — including
    /// its transitions, which become the redemption's conditions — and
    /// return its id and bearer key.
    async fn approve_pending(&self) -> (RequestId, String) {
        let pending = self.kernel.approvals().pending();
        assert_eq!(pending.len(), 1, "exactly one request must be pending");
        let view = pending[0].clone();
        self.authority
            .grant(
                &view.id,
                GrantTerms::once_for_view(
                    &view,
                    std::time::SystemTime::now() + std::time::Duration::from_secs(300),
                ),
            )
            .await
            .expect("the grant must post");
        let token = self
            .authority
            .token_for(&view.id)
            .expect("a credential for a granted request")
            .reveal()
            .to_string();
        (view.id, token)
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

    /// The `observed` set the one `Redeemed` entry recorded, if there is one.
    fn redeemed_observations(&self) -> Option<Vec<kaish_types::approval::Observation>> {
        self.kernel.approvals().log(0).into_iter().find_map(|e| match e {
            LedgerEntry::Redeemed { observed, .. } => Some(observed),
            _ => None,
        })
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
        _ => "other",
    }
}

/// A kernel with the enforce policy and trash forced OFF regardless of the
/// developer's `KAISH_APPROVALS`/`KAISH_TRASH` env, which `repl()` reads.
/// Each test opts in via `set -o approvals`.
fn kernel_at(dir: &Path) -> Session {
    let config = KernelConfig::repl()
        .with_cwd(dir.to_path_buf())
        .with_approvals(false)
        .with_trash(false);
    let (kernel, authority) = Kernel::build(config).expect("kernel");
    Session { kernel, authority }
}

// ============================================================================
// The `path` resolver — `cas_overwrite`'s snapshot-compare, on the ledger
// ============================================================================

/// The test the whole PR exists for. A gated overwrite records the target's
/// digest at gate time; the file changes while the operator is deciding; the
/// redemption refuses, voids the grant, and the file keeps the content the
/// third party wrote.
///
/// This is the existing `cas_overwrite` conflict test re-expressed against
/// the ledger: the same protection, but now it also covers the *gated*
/// target, which previously wrote with no compare at all because only the
/// trash path carried a byte snapshot.
#[tokio::test]
async fn a_file_changed_between_grant_and_redemption_refuses_and_is_not_written() {
    let dir = tempdir();
    std::fs::write(dir.path().join("dst.txt"), "as the operator saw it").unwrap();
    let session = kernel_at(dir.path());
    session.run("set -o approvals").await;

    let gated = session.run("write dst.txt 'the approved content'").await;
    assert_eq!(gated.code, 2, "the overwrite must gate: {}", gated.err);
    let (id, token) = session.approve_pending().await;

    // The world moves while the operator is deciding.
    std::fs::write(dir.path().join("dst.txt"), "someone else got here first").unwrap();

    let refused = session
        .run(&format!("write --confirm={token} dst.txt 'the approved content'"))
        .await;
    assert_eq!(refused.code, 1, "a stale grant must fail loud: {}", refused.err);
    assert!(
        refused.err.contains("refused") && refused.err.contains("changed since the grant"),
        "the message must say the resource moved, not just that it failed: {}",
        refused.err
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("dst.txt")).unwrap(),
        "someone else got here first",
        "the refused write must not have landed"
    );
    assert_eq!(
        session.kernel.approvals().state(&id),
        Some(RequestState::Voided),
        "a refusal voids the grant — the operation must re-request"
    );
    assert_eq!(
        session.entry_kinds(),
        vec!["Requested", "Granted", "KeyRetrieved", "Refused", "Voided"],
        "the record must carry the refusal and the void"
    );
}

/// A refusal reserves nothing. There is no attempt to settle, no attempt in
/// the chain, and the next `redeem` cannot trip `AttemptInFlight` on a
/// phantom reservation.
#[tokio::test]
async fn a_refused_redemption_reserves_no_attempt() {
    let dir = tempdir();
    std::fs::write(dir.path().join("dst.txt"), "before").unwrap();
    let session = kernel_at(dir.path());
    session.run("set -o approvals").await;

    session.run("write dst.txt 'after'").await;
    let (id, token) = session.approve_pending().await;
    std::fs::write(dir.path().join("dst.txt"), "moved").unwrap();
    session
        .run(&format!("write --confirm={token} dst.txt 'after'"))
        .await;

    let chain = session.kernel.approvals().get(&id).expect("the chain");
    assert!(
        chain.attempts.is_empty(),
        "a refused redemption must reserve no AttemptId: {:?}",
        chain.attempts
    );
    assert!(
        !session.entry_kinds().contains(&"Redeemed"),
        "no Redeemed entry may exist for a refusal: {:?}",
        session.entry_kinds()
    );
}

/// The hazard `cas_overwrite` documents (`context.rs`'s "propagate a re-read
/// failure loudly"), generalized: a prior state the resolver cannot read
/// refuses, and never passes as "unspecified".
///
/// The target is replaced by a directory between the grant and the
/// redemption. `exists` still says yes, so nothing short-circuits — the
/// resolver reaches the digest and reports that a directory has none.
///
/// `Kernel::confirm` is the vehicle because it observes before it
/// dispatches, so this reaches the resolver on the redemption path. A
/// `--confirm=<token>` re-run refuses one step earlier, at the gate site's
/// own digest of the fresh draft; both are loud and neither writes.
#[tokio::test]
async fn a_resolver_error_refuses_rather_than_passing() {
    let dir = tempdir();
    std::fs::write(dir.path().join("dst.txt"), "a file, for now").unwrap();
    let session = kernel_at(dir.path());
    session.run("set -o approvals").await;

    session.run("write dst.txt 'the approved content'").await;
    let (id, _token) = session.approve_pending().await;

    std::fs::remove_file(dir.path().join("dst.txt")).unwrap();
    std::fs::create_dir(dir.path().join("dst.txt")).unwrap();

    let refused = session
        .kernel
        .confirm(&session.authority, &id)
        .await
        .expect("confirm executes");
    assert_eq!(
        refused.code, 1,
        "an unobservable precondition must refuse, not proceed: {}",
        refused.err
    );
    assert!(
        refused.err.contains("could not be observed")
            && refused.err.contains("a directory has no content digest"),
        "the message must name the observation failure: {}",
        refused.err
    );
    assert_eq!(
        session.kernel.approvals().state(&id),
        Some(RequestState::Voided)
    );
    assert!(
        dir.path().join("dst.txt").is_dir(),
        "nothing may have been written over the directory"
    );
    assert!(
        session.entry_kinds().contains(&"Refused"),
        "the record must carry the refusal: {:?}",
        session.entry_kinds()
    );
}

/// The `Redeemed` entry is the audit record of the check: it says what was
/// seen and when. Without it, "the condition held" is an unverifiable claim.
#[tokio::test]
async fn the_redeemed_entry_records_what_was_observed_and_when() {
    let dir = tempdir();
    std::fs::write(dir.path().join("dst.txt"), "before").unwrap();
    let session = kernel_at(dir.path());
    session.run("set -o approvals").await;

    let before = std::time::SystemTime::now();
    session.run("write dst.txt 'after'").await;
    let (_id, token) = session.approve_pending().await;
    let done = session
        .run(&format!("write --confirm={token} dst.txt 'after'"))
        .await;
    assert_eq!(done.code, 0, "an unchanged file must redeem: {}", done.err);

    let observed = session
        .redeemed_observations()
        .expect("a successful redemption must append Redeemed");
    assert_eq!(observed.len(), 1, "one path resource, one observation: {observed:?}");
    let observation = &observed[0];
    assert_eq!(observation.resource.kind, "path");
    assert_eq!(observation.resource.id, "dst.txt");
    match &observation.claim {
        StateClaim::Digest { alg, hex } => {
            assert_eq!(alg, "sha256", "the algorithm must be on the record");
            // sha256("before")
            assert_eq!(
                hex,
                "6db7d803e74f1ffa7d8f5adc0bf95b3e15bf4c8373fffadf546227cc6c6742cb",
                "the recorded digest must be the content that was checked"
            );
        }
        other => panic!("the path resolver must record a content digest, got {other:?}"),
    }
    assert!(
        observation.at >= before,
        "the observation must be timestamped when it was taken"
    );
}

/// A grant whose conditions claim nothing redeems, and the record says it
/// was unconditioned — an empty observation set, not a fabricated one.
/// `rm` is the in-tree producer of that shape: a delete declares no prior
/// state, because digesting a whole tree per gate would cost a full read per
/// path.
#[tokio::test]
async fn an_unconditioned_grant_redeems_and_the_record_shows_it() {
    let dir = tempdir();
    std::fs::write(dir.path().join("doomed.txt"), "data").unwrap();
    let session = kernel_at(dir.path());
    session.run("set -o approvals").await;

    let gated = session.run("rm doomed.txt").await;
    assert_eq!(gated.code, 2, "rm must gate: {}", gated.err);
    let (id, token) = session.approve_pending().await;

    let grant = session
        .kernel
        .approvals()
        .get(&id)
        .and_then(|c| c.grant)
        .expect("the grant");
    assert!(
        grant.conditions.iter().all(|c| c.expected_from == StateClaim::Unspecified),
        "rm's resources declare no prior state: {:?}",
        grant.conditions
    );

    let done = session
        .run(&format!("rm --confirm={token} doomed.txt"))
        .await;
    assert_eq!(done.code, 0, "an unconditioned grant must redeem: {}", done.err);
    assert!(!dir.path().join("doomed.txt").exists(), "the delete must have run");
    assert_eq!(
        session.redeemed_observations(),
        Some(Vec::new()),
        "an unconditioned redemption records an empty observation set, which is what \
         tells an auditor nothing was claimed"
    );
}

// ============================================================================
// A non-`path` kind, end to end — the shape kaish-git ships
// ============================================================================

/// A `git.ref` resolver over an in-memory ref store, so the test can move
/// the ref between the grant and the redemption the way a concurrent push
/// would.
struct GitRefResolver {
    refs: Arc<Mutex<Vec<(String, String)>>>,
    /// When set, every `observe` fails — the plugin-side flavor of the
    /// unobservable case.
    fail: bool,
    observed: Arc<AtomicUsize>,
}

#[async_trait]
impl StateResolver for GitRefResolver {
    fn kind(&self) -> &str {
        "git.ref"
    }

    async fn observe(&self, id: &str) -> Result<StateClaim, ResolverError> {
        self.observed.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(ResolverError::Io(format!("{id}: the ref store is unreadable")));
        }
        let refs = self.refs.lock().unwrap();
        Ok(refs
            .iter()
            .find(|(name, _)| name == id)
            .map(|(_, oid)| StateClaim::Exact(oid.clone()))
            .unwrap_or(StateClaim::Absent))
    }
}

/// `git-push`: a plugin-shaped gate producer. Declares the ref transition it
/// intends, reading the current oid from the same store the resolver reads —
/// which is what a real `git push` does with `gix`.
struct GitPush {
    refs: Arc<Mutex<Vec<(String, String)>>>,
    pushed: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for GitPush {
    fn name(&self) -> &str {
        "git-push"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("git-push", "test tool: gate a ref update on its prior oid")
    }

    async fn execute(&self, _args: ToolArgs, ctx: &mut dyn ToolCtx) -> ExecResult {
        let current = {
            let refs = self.refs.lock().unwrap();
            refs.iter()
                .find(|(name, _)| name == "refs/heads/main")
                .map(|(_, oid)| oid.clone())
                .expect("the fixture seeds refs/heads/main")
        };
        let draft = ApprovalRequest::builder("git.push")
            .risk(kaish_types::approval::RiskClass::Irreversible)
            .reason("pushing to a protected ref")
            .hint("git-push --confirm=<token>")
            .resource(Resource::transition(
                "git.ref",
                "refs/heads/main",
                StateClaim::Exact(current),
                StateClaim::Exact("c3d4".to_string()),
            ))
            .build()
            .expect("a well-formed draft");
        if let Err(result) = ctx.request_approval(draft).await.proceed() {
            return result;
        }
        self.pushed.fetch_add(1, Ordering::SeqCst);
        let mut refs = self.refs.lock().unwrap();
        for entry in refs.iter_mut() {
            if entry.0 == "refs/heads/main" {
                entry.1 = "c3d4".to_string();
            }
        }
        ExecResult::success("pushed")
    }
}

struct GitFixture {
    session: Session,
    refs: Arc<Mutex<Vec<(String, String)>>>,
    pushed: Arc<AtomicUsize>,
    observed: Arc<AtomicUsize>,
}

fn git_session(dir: &Path, failing_resolver: bool) -> GitFixture {
    let refs = Arc::new(Mutex::new(vec![(
        "refs/heads/main".to_string(),
        "a1b2".to_string(),
    )]));
    let pushed = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(AtomicUsize::new(0));
    let (_r, _a, authority) = Ledger::build(LedgerConfig::default(), None).expect("ledger");
    let config = KernelConfig::isolated()
        .with_cwd(dir.to_path_buf())
        .with_approvals(false)
        .with_trash(false)
        .with_approver_handle(authority.clone())
        .with_state_resolver(Arc::new(GitRefResolver {
            refs: Arc::clone(&refs),
            fail: failing_resolver,
            observed: Arc::clone(&observed),
        }));
    let kernel = build_tool_kernel(config, Arc::clone(&refs), Arc::clone(&pushed));
    GitFixture {
        session: Session { kernel, authority },
        refs,
        pushed,
        observed,
    }
}

/// A `MemoryFs`-backed kernel with `git-push` registered. `with_backend` is
/// the only constructor that takes a tool closure, so the authority is minted
/// outside and adopted through the config — which is also how a real embedder
/// shares one ledger across sessions.
fn build_tool_kernel(
    config: KernelConfig,
    refs: Arc<Mutex<Vec<(String, String)>>>,
    pushed: Arc<AtomicUsize>,
) -> Kernel {
    let mut vfs = VfsRouter::new();
    vfs.mount("/", MemoryFs::new());
    let backend: Arc<dyn KernelBackend> = Arc::new(LocalBackend::new(Arc::new(vfs)));
    Kernel::with_backend(backend, config, |_| {}, move |tools| {
        tools.register(GitPush { refs, pushed });
    })
    .expect("kernel")
}

/// A registered resolver for a plugin's own kind carries the whole cycle:
/// the request declares `refs/heads/main: a1b2 → c3d4`, the grant copies
/// that into a condition, and the replay re-reads the ref through the
/// plugin's resolver before the push runs.
#[tokio::test]
async fn a_plugin_resource_kind_verifies_end_to_end() {
    let dir = tempdir();
    let fixture = git_session(dir.path(), false);

    let gated = fixture.session.run("git-push").await;
    assert_eq!(gated.code, 2, "the push must gate: {}", gated.err);
    assert_eq!(fixture.pushed.load(Ordering::SeqCst), 0, "nothing may have pushed yet");
    let (id, _token) = fixture.session.approve_pending().await;

    let done = fixture
        .session
        .kernel
        .confirm(&fixture.session.authority, &id)
        .await
        .expect("confirm executes");
    assert_eq!(done.code, 0, "an unmoved ref must redeem: {}", done.err);
    assert_eq!(fixture.pushed.load(Ordering::SeqCst), 1, "the push must have run once");
    assert!(
        fixture.observed.load(Ordering::SeqCst) >= 1,
        "the plugin's resolver must have been consulted"
    );

    let observed = fixture
        .session
        .redeemed_observations()
        .expect("Redeemed must be on the log");
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].resource.kind, "git.ref");
    assert_eq!(observed[0].claim, StateClaim::Exact("a1b2".to_string()));
}

/// The same cycle with the ref moved while the operator was deciding: the
/// push does not happen and the record says exactly why.
#[tokio::test]
async fn a_plugin_resource_that_moved_refuses_the_replay() {
    let dir = tempdir();
    let fixture = git_session(dir.path(), false);

    fixture.session.run("git-push").await;
    let (id, _token) = fixture.session.approve_pending().await;

    // Someone else pushed first.
    fixture.refs.lock().unwrap()[0].1 = "e5f6".to_string();

    let refused = fixture
        .session
        .kernel
        .confirm(&fixture.session.authority, &id)
        .await
        .expect("confirm executes");
    assert_eq!(refused.code, 1, "a moved ref must refuse: {}", refused.err);
    assert!(
        refused.err.contains("git.ref:refs/heads/main changed since the grant"),
        "the message must name the resource that moved: {}",
        refused.err
    );
    assert_eq!(fixture.pushed.load(Ordering::SeqCst), 0, "the push must not have run");
    assert_eq!(
        fixture.session.kernel.approvals().state(&id),
        Some(RequestState::Voided)
    );
    assert_eq!(fixture.refs.lock().unwrap()[0].1, "e5f6", "the ref must be untouched");
}

/// A plugin resolver that cannot read its store refuses too — the same rule
/// the `path` resolver follows, proven on the surface a plugin owns.
#[tokio::test]
async fn a_failing_plugin_resolver_refuses_the_replay() {
    let dir = tempdir();
    let fixture = git_session(dir.path(), true);

    // The gate's own decision defers before any observation, so the request
    // posts even with the resolver broken.
    let gated = fixture.session.run("git-push").await;
    assert_eq!(gated.code, 2, "the push must gate: {}", gated.err);
    let (id, _token) = fixture.session.approve_pending().await;

    let refused = fixture
        .session
        .kernel
        .confirm(&fixture.session.authority, &id)
        .await
        .expect("confirm executes");
    assert_eq!(refused.code, 1, "an unreadable store must refuse: {}", refused.err);
    assert!(
        refused.err.contains("could not be observed")
            && refused.err.contains("the ref store is unreadable"),
        "the resolver's own words must survive to the operator: {}",
        refused.err
    );
    assert_eq!(fixture.pushed.load(Ordering::SeqCst), 0, "the push must not have run");
}

/// A resource kind with no registered resolver refuses. The alternative —
/// treating "nobody can check this" as "it passes" — is the exact silent
/// fallback the spec forbids.
#[tokio::test]
async fn an_unregistered_resource_kind_refuses() {
    let dir = tempdir();
    let refs = Arc::new(Mutex::new(vec![(
        "refs/heads/main".to_string(),
        "a1b2".to_string(),
    )]));
    let pushed = Arc::new(AtomicUsize::new(0));
    // No `with_state_resolver` — the kernel has never heard of `git.ref`.
    let (_r, _a, authority) = Ledger::build(LedgerConfig::default(), None).expect("ledger");
    let config = KernelConfig::isolated()
        .with_cwd(dir.path().to_path_buf())
        .with_approvals(false)
        .with_trash(false)
        .with_approver_handle(authority.clone());
    let kernel = build_tool_kernel(config, Arc::clone(&refs), Arc::clone(&pushed));
    let session = Session { kernel, authority };

    session.run("git-push").await;
    let (id, _token) = session.approve_pending().await;
    let refused = session
        .kernel
        .confirm(&session.authority, &id)
        .await
        .expect("confirm executes");
    assert_eq!(refused.code, 1, "an uncheckable claim must refuse: {}", refused.err);
    assert!(
        refused.err.contains("no state resolver is registered for the 'git.ref' resource kind"),
        "the message must name what is missing: {}",
        refused.err
    );
    assert_eq!(pushed.load(Ordering::SeqCst), 0);
}

/// `path` belongs to the kernel. A resolver claiming it fails
/// `Kernel::build` rather than shadowing the one that decides whether a file
/// changed.
#[test]
fn registering_a_path_resolver_fails_the_build() {
    struct Shadow;

    #[async_trait]
    impl StateResolver for Shadow {
        fn kind(&self) -> &str {
            "path"
        }
        async fn observe(&self, _id: &str) -> Result<StateClaim, ResolverError> {
            Ok(StateClaim::Unspecified)
        }
    }

    let dir = tempdir();
    let config = KernelConfig::repl()
        .with_cwd(dir.path().to_path_buf())
        .with_state_resolver(Arc::new(Shadow));
    let rendered = match Kernel::build(config) {
        Ok(_) => panic!("a 'path' resolver must be refused"),
        Err(err) => format!("{err:#}"),
    };
    assert!(
        rendered.contains("belongs to the kernel"),
        "the failure must say why: {rendered}"
    );
}

/// A `ToolCtx`-only tool's `ExecContext` is what the gate reaches through,
/// so the digest the resolver sees is the one the backend serves — this
/// keeps the harness honest that `ExecContext` is the real path.
#[tokio::test]
async fn the_path_resolver_reads_through_the_backend() {
    let dir = tempdir();
    std::fs::write(dir.path().join("t.txt"), "content").unwrap();
    let session = kernel_at(dir.path());
    session.run("set -o approvals").await;
    session.run("write t.txt 'replacement'").await;

    let pending = session.kernel.approvals().pending();
    let resource = &pending[0].resources[0];
    assert_eq!(resource.kind, "path");
    let transition = resource
        .transition
        .as_ref()
        .expect("a gated overwrite declares its prior digest");
    match &transition.from {
        StateClaim::Digest { alg, .. } => assert_eq!(alg, "sha256"),
        other => panic!("expected a content digest, got {other:?}"),
    }
    assert_eq!(
        transition.to,
        StateClaim::Unspecified,
        "the resulting content is not claimed — `patch` and `sed -i` cannot know it"
    );
}
