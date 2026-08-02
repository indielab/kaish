//! `ToolCtx::request_approval` — the kernel's real implementation
//! (`docs/approval-ledger.md` §C.1, §D.1, ledger PR 3).
//!
//! No `#![cfg(feature = ...)]` gate: `ExecContext::new` here mounts a
//! `MemoryFs` (no real filesystem, no `localfs` needed), and the ledger
//! itself has no OS dependency, so this file compiles and passes
//! featureless.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use kaish_kernel::ledger::{Ledger, LedgerConfig};
use kaish_kernel::vfs::{MemoryFs, VfsRouter};
use kaish_kernel::{ExecContext, LedgerAccess};
use kaish_tool_api::{ApprovalOutcome, Tool, ToolArgs, ToolCtx};
use kaish_types::approval::{
    ApprovalRequest, Capture, GrantTerms, Outcome, Principal, PrincipalKind, RequestState, RiskClass,
};

fn ctx_with_memory_fs() -> ExecContext {
    let mut vfs = VfsRouter::new();
    vfs.mount("/", MemoryFs::new());
    ExecContext::new(Arc::new(vfs))
}

fn agent(id: &str) -> Principal {
    Principal::new(id, PrincipalKind::Agent)
}

#[tokio::test]
async fn kernel_request_approval_round_trips_a_request_through_the_ledger() {
    let (requester, approvals, _approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let mut ctx = ctx_with_memory_fs();
    ctx.ledger_access = Some(LedgerAccess {
        requester,
        approvals: approvals.clone(),
        principal: agent("agent-1"),
        request_ttl: Duration::from_secs(60),
    });

    let draft = ApprovalRequest::builder("plugin.dangerous")
        .risk(RiskClass::Irreversible)
        .reason("round-trip test")
        .build()
        .unwrap();

    let outcome = ctx.request_approval(draft).await;
    let view = match outcome {
        ApprovalOutcome::Pending(view) => *view,
        other => panic!("PR 3 wires no decision chain — every post must defer to Pending, got {other:?}"),
    };
    assert_eq!(view.operation.as_str(), "plugin.dangerous");
    assert_eq!(
        view.principal,
        agent("agent-1"),
        "the stamped principal must be the context's own, not left default"
    );
    assert_eq!(
        view.capture,
        Capture::DirectExecution,
        "no dispatch seam ran above this call — it must capture as DirectExecution, not a fabricated Exact"
    );

    // The ledger itself has the request, independent of the view returned
    // to the caller.
    assert_eq!(approvals.state(&view.id), Some(RequestState::Requested));
}

#[tokio::test]
async fn kernel_request_approval_with_no_ledger_wired_is_unsupported() {
    // Today's only production value: no `KernelConfig::with_ledger` exists
    // yet (PR 4), so `ledger_access` is always `None` outside a test.
    let mut ctx = ctx_with_memory_fs();
    assert!(ctx.ledger_access.is_none());

    let draft = ApprovalRequest::builder("plugin.dangerous")
        .risk(RiskClass::Irreversible)
        .build()
        .unwrap();
    let outcome = ctx.request_approval(draft).await;
    assert!(matches!(outcome, ApprovalOutcome::Unsupported));
}

/// Fixture tool depending on ONLY `kaish-tool-api` (plus the `kaish-types`
/// vocabulary that crate already re-exports/depends on) — the acceptance
/// criterion for ledger PR 3 (spec §H): "if the fixture needs
/// `kaish-kernel` or `as_any_mut`, the PR is not done." Every `use` inside
/// this module names `kaish_tool_api`/`kaish_types`/`async_trait` only; the
/// surrounding test (which drives a real kernel `ExecContext` and ledger)
/// is not part of the fixture itself.
mod plugin_dangerous {
    use async_trait::async_trait;
    use kaish_tool_api::{ExecResult, Tool, ToolArgs, ToolCtx, ToolSchema};
    use kaish_types::approval::{ApprovalRequest, RiskClass};

    /// A synthetic `plugin.dangerous` operation — stands in for a real
    /// out-of-tree plugin (kaish-git's own gated operations) that has no
    /// `kaish-kernel` dependency at all.
    pub struct PluginDangerous;

    #[async_trait]
    impl Tool for PluginDangerous {
        fn name(&self) -> &str {
            "plugin-dangerous"
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema::new("plugin-dangerous", "fixture: gates a synthetic dangerous operation")
        }

        async fn execute(&self, _args: ToolArgs, ctx: &mut dyn ToolCtx) -> ExecResult {
            let draft = match ApprovalRequest::builder("plugin.dangerous")
                .risk(RiskClass::Irreversible)
                .reason("fixture: gates a synthetic dangerous operation")
                .hint("plugin-dangerous --confirm=<token>")
                .build()
            {
                Ok(draft) => draft,
                Err(e) => return ExecResult::failure(1, e.to_string()),
            };
            // The one call pattern (spec §C.1) — the entire gate.
            match ctx.request_approval(draft).await.proceed() {
                Ok(_attempt) => ExecResult::success("dangerous operation performed"),
                Err(result) => result,
            }
        }
    }
}

#[tokio::test]
async fn plugin_dangerous_fixture_gates_end_to_end_through_tool_api_alone() {
    use plugin_dangerous::PluginDangerous;

    let (requester, approvals, approver) = Ledger::build(LedgerConfig::default(), None).unwrap();
    let mut ctx = ctx_with_memory_fs();
    ctx.ledger_access = Some(LedgerAccess {
        requester: requester.clone(),
        approvals: approvals.clone(),
        principal: agent("agent-1"),
        request_ttl: Duration::from_secs(60),
    });
    let tool = PluginDangerous;

    // 1. Request + defer: the fixture's one call posts to the ledger and
    //    returns exit 2. Nothing above this line or in the fixture itself
    //    names `kaish-kernel`.
    let result = tool.execute(ToolArgs::new(), &mut ctx).await;
    assert_eq!(result.code, 2, "a deferred request must be exit 2, not a bare failure");
    let view = result
        .approval_request()
        .expect("Pending must post the view on ExecResult's control-plane field");
    assert_eq!(view.operation.as_str(), "plugin.dangerous");
    assert_eq!(approvals.state(&view.id), Some(RequestState::Requested));

    // 2. Out-of-band grant, via the approval side only — spec §D.3: exactly
    //    one bridge from script/tool code to `grant` exists (the
    //    `approvals` builtin, PR 7), and a plugin has no path to this
    //    `ApproverHandle` at all. `GrantTerms::once_for` needs an
    //    `ApprovalRequest`, not the tokenless view the fixture got back;
    //    re-stamping an equivalent one from the view's own fields (which
    //    are exactly the request's fields minus the credential) is the
    //    legitimate way to reach it from outside the ledger crate.
    let terms_source = ApprovalRequest::builder("plugin.dangerous")
        .risk(RiskClass::Irreversible)
        .build()
        .unwrap()
        .stamp(
            view.id.clone(),
            view.principal.clone(),
            view.capture.clone(),
            view.context.clone(),
            view.requested_at,
            view.ttl,
            view.job_id,
        );
    let not_after = SystemTime::now() + Duration::from_secs(300);
    approver
        .grant(&view.id, GrantTerms::once_for(&terms_source, not_after))
        .await
        .unwrap();
    assert_eq!(approvals.state(&view.id), Some(RequestState::Granted));

    // 3. "Confirm-replay": `Kernel::confirm` (the automated replay of the
    //    captured invocation) is ledger PR 5 — it needs the `Capture`-based
    //    draft matcher to correlate a fresh re-invocation back to this
    //    request, which has no gate site to call it from yet (see this
    //    PR's decisions list). Driven directly through the ledger handles
    //    here, standing in for what PR 5 will automate.
    let attempt = requester.redeem(&view.id, agent("agent-1"), Vec::new()).await.unwrap();

    // 4. Settle.
    let appended = requester.settle(&attempt, Outcome::Exit(0)).await.unwrap();
    assert!(appended, "settle must append — this is the attempt's first report");

    let chain = approvals.get(&view.id).expect("chain must still exist after settlement");
    assert_eq!(chain.attempts.len(), 1);
    assert_eq!(chain.attempts[0].outcome, Some(Outcome::Exit(0)));
}
