//! `ToolSchema.operations` (`docs/approval-ledger.md` §F.3 item 5, ledger PR
//! "spec-gaps" item 3): the dotted operation ids a tool can post through the
//! approval ledger, so a policy engine can discover gateable operations from
//! `kaish-tools --json` instead of sniffing for a `--confirm` flag.
//!
//! No `#![cfg(feature = ...)]` gate: `register_builtins` and
//! `KernelConfig::isolated()` need no real filesystem, and `KernelOperation`
//! has no OS dependency either — everything here compiles and passes
//! featureless (`--no-default-features`).

// Test-fixture code: unwrap/expect on known-good setup is the idiom here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use kaish_kernel::ledger::KernelOperation;
use kaish_kernel::tools::{register_builtins, ToolRegistry, ToolSchema};
use kaish_kernel::{Kernel, KernelConfig};

fn all_kernel_operation_ids() -> Vec<&'static str> {
    vec![
        KernelOperation::FsRemove.as_str(),
        KernelOperation::FsOverwrite.as_str(),
        KernelOperation::FsRename.as_str(),
        KernelOperation::TrashEmpty.as_str(),
    ]
}

/// Recurse into `schema.subcommands` too — none of today's gate producers
/// are subcommand trees, but the drift guard must not go blind the day one
/// is.
fn collect_operations(schema: &ToolSchema, out: &mut Vec<(String, String)>) {
    for op in &schema.operations {
        out.push((schema.name.clone(), op.clone()));
    }
    for child in &schema.subcommands {
        collect_operations(child, out);
    }
}

/// The drift guard spec asked for: "a test asserting every declared
/// operation string parses/matches a `KernelOperation` or registered
/// namespace." Every in-tree producer's declared operation must be one of
/// the closed enum's own ids — a typo or a stale string here would let
/// `tools --json` advertise an operation the kernel can never actually post.
#[test]
fn every_declared_operation_matches_a_kernel_operation() {
    let mut registry = ToolRegistry::new();
    register_builtins(&mut registry);
    let known = all_kernel_operation_ids();

    let mut declared: Vec<(String, String)> = Vec::new();
    for schema in registry.schemas() {
        collect_operations(&schema, &mut declared);
    }

    for (tool, op) in &declared {
        assert!(
            known.contains(&op.as_str()),
            "{tool} declares operation {op:?}, which is not one of KernelOperation's own ids {known:?}"
        );
    }

    // The inverse check: prove this isn't vacuously trivial by pinning the
    // exact producer -> operation mapping spec §F.3 item 5 names (rm ->
    // fs.remove, the seven gate_overwrites callers -> fs.overwrite except
    // mv -> fs.rename, kaish-trash -> trash.empty).
    let by_tool: BTreeMap<&str, Vec<&str>> = declared
        .iter()
        .fold(BTreeMap::new(), |mut map, (tool, op)| {
            map.entry(tool.as_str()).or_default().push(op.as_str());
            map
        });

    assert_eq!(by_tool.get("rm"), Some(&vec!["fs.remove"]));
    assert_eq!(by_tool.get("mv"), Some(&vec!["fs.rename"]));
    assert_eq!(by_tool.get("kaish-trash"), Some(&vec!["trash.empty"]));
    for tool in ["cp", "dd", "patch", "sed", "tee", "write"] {
        assert_eq!(
            by_tool.get(tool),
            Some(&vec!["fs.overwrite"]),
            "{tool} must declare fs.overwrite"
        );
    }

    // A tool that gates nothing must declare nothing — `operations` names
    // what a tool *can* post, not a blanket "every builtin is interesting"
    // list.
    assert!(
        !by_tool.contains_key("cat"),
        "cat never gates anything and must declare no operations"
    );
}

#[tokio::test]
async fn kaish_tools_json_surfaces_operations_as_a_real_array() {
    let (kernel, _authority) = Kernel::build(KernelConfig::isolated()).expect("kernel build");
    let result = kernel.execute("kaish-tools --json").await.expect("execute");
    assert_eq!(result.code, 0, "kaish-tools --json failed: {}", result.err);

    let text = result.text_out();
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("kaish-tools --json must be valid JSON");
    let array = parsed.as_array().expect("top-level JSON must be an array");

    let rm = array
        .iter()
        .find(|v| v.get("name").and_then(|n| n.as_str()) == Some("rm"))
        .expect("rm must be listed");
    let rm_ops: Vec<&str> = rm
        .get("operations")
        .and_then(|o| o.as_array())
        .expect("rm's operations must be a JSON array, not a joined string")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        rm_ops,
        vec!["fs.remove"],
        "rm must advertise fs.remove through kaish-tools --json"
    );

    // A non-gating tool must not fabricate an operations array.
    let cat = array
        .iter()
        .find(|v| v.get("name").and_then(|n| n.as_str()) == Some("cat"))
        .expect("cat must be listed");
    let cat_ops = cat.get("operations").and_then(|o| o.as_array()).map(|a| a.len()).unwrap_or(0);
    assert_eq!(cat_ops, 0, "cat never gates anything and must declare no operations");
}
