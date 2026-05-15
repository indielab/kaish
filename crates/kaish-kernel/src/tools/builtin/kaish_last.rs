//! kaish-last — Dump the previous command's result as text.
//!
//! Rules:
//! - If `.data` is set on the previous result, emit it as JSON.
//! - Else if `.out` is non-empty (captured stdout), emit it verbatim.
//! - Else exit 1 with "no data" on stderr.
//!
//! Refuses to run with piped stdin (Middle or Last in a pipeline):
//! pipeline stages don't see the previous stage's ExecResult, only the
//! kernel's pre-pipeline `last_result`, so `seq 1 5 | kaish-last` would
//! silently surface whatever ran before the pipeline. Producing into a
//! pipe (`kaish-last | jq`) is the intended use case and stays allowed.

use async_trait::async_trait;

use crate::dispatch::PipelinePosition;
use crate::interpreter::{ExecResult, OutputData};
use crate::tools::{ExecContext, Tool, ToolArgs, ToolSchema};
use kaish_types::value_to_json;

pub struct KaishLast;

#[async_trait]
impl Tool for KaishLast {
    fn name(&self) -> &str {
        "kaish-last"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "kaish-last",
            "Dump the previous command's structured data (or stdout) as text",
        )
        .example("Pipe structured data through jq", "seq 1 5\nkaish-last | jq '.[2]'")
        .example("Capture for later use", "seq 1 5\nDATA=$(kaish-last)")
    }

    async fn execute(&self, _args: ToolArgs, ctx: &mut ExecContext) -> ExecResult {
        // Receiving piped stdin would mean reading the kernel's pre-pipeline
        // last_result, not the previous stage's — silently wrong. Refuse.
        // Producing into a pipe (First/Only) is fine.
        if matches!(
            ctx.pipeline_position,
            PipelinePosition::Middle | PipelinePosition::Last
        ) {
            return ExecResult::failure(
                2,
                "kaish-last: cannot receive piped stdin (it reads the previous statement's result, not the previous pipeline stage); run as a standalone statement or as the first stage of a pipeline",
            );
        }

        let prev = ctx.scope.last_result();

        if let Some(ref data) = prev.data {
            let json = value_to_json(data);
            return ExecResult::with_output(OutputData::text(format!("{}\n", json)));
        }

        let out = prev.text_out();
        if !out.is_empty() {
            return ExecResult::with_output(OutputData::text(out.into_owned()));
        }

        ExecResult::failure(1, "kaish-last: no data from previous command")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Value;
    use crate::interpreter::ExecResult;
    use crate::vfs::{MemoryFs, VfsRouter};
    use std::sync::Arc;

    fn make_ctx() -> ExecContext {
        let mut vfs = VfsRouter::new();
        vfs.mount("/", MemoryFs::new());
        ExecContext::new(Arc::new(vfs))
    }

    #[tokio::test]
    async fn emits_data_as_json_when_set() {
        let mut ctx = make_ctx();
        ctx.scope.set_last_result(ExecResult::success_with_data(
            "1\n2\n3\n",
            Value::Json(serde_json::json!([1, 2, 3])),
        ));

        let result = KaishLast.execute(ToolArgs::new(), &mut ctx).await;
        assert!(result.ok());
        assert_eq!(result.text_out().trim(), "[1,2,3]");
    }

    #[tokio::test]
    async fn falls_back_to_stdout_when_no_data() {
        let mut ctx = make_ctx();
        ctx.scope
            .set_last_result(ExecResult::success("hello world\n"));

        let result = KaishLast.execute(ToolArgs::new(), &mut ctx).await;
        assert!(result.ok());
        assert_eq!(result.text_out().as_ref(), "hello world\n");
    }

    #[tokio::test]
    async fn exits_one_when_no_data_and_no_stdout() {
        let mut ctx = make_ctx();
        ctx.scope.set_last_result(ExecResult::default());

        let result = KaishLast.execute(ToolArgs::new(), &mut ctx).await;
        assert!(!result.ok());
        assert_eq!(result.code, 1);
        assert!(result.err.contains("no data"));
    }

    #[tokio::test]
    async fn refuses_with_piped_stdin() {
        let mut ctx = make_ctx();
        ctx.pipeline_position = PipelinePosition::Last;
        ctx.scope.set_last_result(ExecResult::success_with_data(
            "x",
            Value::Json(serde_json::json!([1])),
        ));

        let result = KaishLast.execute(ToolArgs::new(), &mut ctx).await;
        assert!(!result.ok());
        assert_eq!(result.code, 2);
        assert!(result.err.contains("piped stdin"));
    }

    #[tokio::test]
    async fn allowed_as_first_in_pipeline() {
        let mut ctx = make_ctx();
        ctx.pipeline_position = PipelinePosition::First;
        ctx.scope.set_last_result(ExecResult::success_with_data(
            "x",
            Value::Json(serde_json::json!([1, 2])),
        ));

        let result = KaishLast.execute(ToolArgs::new(), &mut ctx).await;
        assert!(result.ok(), "expected success producing into pipe");
        assert_eq!(result.text_out().trim(), "[1,2]");
    }
}
