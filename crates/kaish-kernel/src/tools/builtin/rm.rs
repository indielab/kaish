//! rm — Remove files and directories.

use async_trait::async_trait;
use std::path::Path;

use crate::interpreter::ExecResult;
use crate::tools::{ExecContext, Tool, ToolArgs, ToolSchema, ParamSchema};
use crate::vfs::Filesystem;

/// Rm tool: remove files and directories.
pub struct Rm;

#[async_trait]
impl Tool for Rm {
    fn name(&self) -> &str {
        "rm"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("rm", "Remove files and directories")
            .param(ParamSchema::required("path", "string", "Path to remove"))
    }

    async fn execute(&self, args: ToolArgs, ctx: &mut ExecContext) -> ExecResult {
        let path = match args.get_string("path", 0) {
            Some(p) => p,
            None => return ExecResult::failure(1, "rm: missing path argument"),
        };

        let resolved = ctx.resolve_path(&path);

        match ctx.vfs.remove(Path::new(&resolved)).await {
            Ok(()) => ExecResult::success(""),
            Err(e) => ExecResult::failure(1, format!("rm: {}: {}", path, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Value;
    use crate::vfs::{Filesystem, MemoryFs, VfsRouter};
    use std::sync::Arc;

    async fn make_ctx() -> ExecContext {
        let mut vfs = VfsRouter::new();
        let mem = MemoryFs::new();
        mem.write(Path::new("file.txt"), b"data").await.unwrap();
        mem.mkdir(Path::new("emptydir")).await.unwrap();
        mem.write(Path::new("fulldir/file.txt"), b"data").await.unwrap();
        vfs.mount("/", mem);
        ExecContext::new(Arc::new(vfs))
    }

    #[tokio::test]
    async fn test_rm_file() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/file.txt".into()));

        let result = Rm.execute(args, &mut ctx).await;
        assert!(result.ok());

        // Verify deleted
        assert!(!ctx.vfs.exists(Path::new("/file.txt")).await);
    }

    #[tokio::test]
    async fn test_rm_empty_dir() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/emptydir".into()));

        let result = Rm.execute(args, &mut ctx).await;
        assert!(result.ok());

        assert!(!ctx.vfs.exists(Path::new("/emptydir")).await);
    }

    #[tokio::test]
    async fn test_rm_non_empty_dir_fails() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/fulldir".into()));

        let result = Rm.execute(args, &mut ctx).await;
        assert!(!result.ok());
        // Directory should still exist
        assert!(ctx.vfs.exists(Path::new("/fulldir")).await);
    }

    #[tokio::test]
    async fn test_rm_nonexistent() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/nonexistent".into()));

        let result = Rm.execute(args, &mut ctx).await;
        assert!(!result.ok());
    }

    #[tokio::test]
    async fn test_rm_no_arg() {
        let mut ctx = make_ctx().await;
        let args = ToolArgs::new();

        let result = Rm.execute(args, &mut ctx).await;
        assert!(!result.ok());
        assert!(result.err.contains("missing"));
    }
}
