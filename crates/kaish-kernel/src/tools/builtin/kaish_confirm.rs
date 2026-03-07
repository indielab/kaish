//! kaish-confirm — Retrieve output cached by the spill latch.
//!
//! When `set -o latch` is active and a command's output exceeds the output
//! limit, kaish exits 2 and issues a nonce. Running `kaish-confirm <nonce>`
//! returns the cached (truncated) result with exit 0.
//!
//! # Examples
//!
//! ```kaish
//! kaish-confirm ab12cd34
//! ```

use async_trait::async_trait;

use crate::interpreter::ExecResult;
use crate::tools::{ExecContext, ParamSchema, Tool, ToolArgs, ToolSchema};

pub struct KaishConfirm;

#[async_trait]
impl Tool for KaishConfirm {
    fn name(&self) -> &str {
        "kaish-confirm"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("kaish-confirm", "Retrieve output cached by the spill latch")
            .param(ParamSchema::required("nonce", "string", "Nonce from the spill latch message"))
            .example("Retrieve truncated output", "kaish-confirm ab12cd34")
    }

    async fn execute(&self, args: ToolArgs, ctx: &mut ExecContext) -> ExecResult {
        let nonce = match args.get_string("nonce", 0) {
            Some(n) => n,
            None => return ExecResult::failure(1, "kaish-confirm: nonce required"),
        };

        match ctx.nonce_store.get_cached_result(&nonce) {
            Some(result) => result,
            None => ExecResult::failure(
                1,
                "kaish-confirm: nonce not found or expired — re-run the original command",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::{MemoryFs, VfsRouter};
    use std::sync::Arc;

    fn make_ctx() -> ExecContext {
        let mut vfs = VfsRouter::new();
        vfs.mount("/", MemoryFs::new());
        ExecContext::new(Arc::new(vfs))
    }

    #[tokio::test]
    async fn test_confirm_valid_nonce() {
        let mut ctx = make_ctx();
        let cached = ExecResult::success("truncated output");
        let nonce = ctx.nonce_store.issue_with_result(cached);

        let mut args = ToolArgs::new();
        args.positional.push(crate::ast::Value::String(nonce));

        let result = KaishConfirm.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert_eq!(result.out, "truncated output");
    }

    #[tokio::test]
    async fn test_confirm_bogus_nonce() {
        let mut ctx = make_ctx();
        let mut args = ToolArgs::new();
        args.positional.push(crate::ast::Value::String("bogus123".into()));

        let result = KaishConfirm.execute(args, &mut ctx).await;
        assert_eq!(result.code, 1);
        assert!(result.err.contains("not found or expired"));
    }

    #[tokio::test]
    async fn test_confirm_missing_nonce() {
        let mut ctx = make_ctx();
        let result = KaishConfirm.execute(ToolArgs::new(), &mut ctx).await;
        assert_eq!(result.code, 1);
        assert!(result.err.contains("nonce required"));
    }
}
