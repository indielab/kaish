//! cd — Change working directory.

use async_trait::async_trait;
use std::path::Path;

use crate::ast::Value;
use crate::interpreter::ExecResult;
use crate::tools::{ExecContext, Tool, ToolArgs, ToolSchema, ParamSchema};
use crate::vfs::Filesystem;

/// Cd tool: change current working directory.
pub struct Cd;

#[async_trait]
impl Tool for Cd {
    fn name(&self) -> &str {
        "cd"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("cd", "Change current working directory")
            .param(ParamSchema::optional(
                "path",
                "string",
                Value::String("/".into()),
                "Directory to change to",
            ))
    }

    async fn execute(&self, args: ToolArgs, ctx: &mut ExecContext) -> ExecResult {
        let path = args
            .get_string("path", 0)
            .unwrap_or_else(|| "/".to_string());

        let resolved = ctx.resolve_path(&path);

        // Verify the path exists and is a directory
        match ctx.vfs.stat(Path::new(&resolved)).await {
            Ok(meta) => {
                if meta.is_dir {
                    ctx.set_cwd(resolved);
                    ExecResult::success("")
                } else {
                    ExecResult::failure(1, format!("cd: {}: Not a directory", path))
                }
            }
            Err(e) => ExecResult::failure(1, format!("cd: {}: {}", path, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::{MemoryFs, VfsRouter};
    use std::path::PathBuf;
    use std::sync::Arc;

    async fn make_ctx() -> ExecContext {
        let mut vfs = VfsRouter::new();
        let mem = MemoryFs::new();
        mem.mkdir(Path::new("subdir")).await.unwrap();
        mem.write(Path::new("file.txt"), b"data").await.unwrap();
        vfs.mount("/", mem);
        ExecContext::new(Arc::new(vfs))
    }

    #[tokio::test]
    async fn test_cd_subdir() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/subdir".into()));

        let result = Cd.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert_eq!(ctx.cwd, PathBuf::from("/subdir"));
    }

    #[tokio::test]
    async fn test_cd_root() {
        let mut ctx = make_ctx().await;
        ctx.set_cwd(PathBuf::from("/subdir"));

        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/".into()));

        let result = Cd.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert_eq!(ctx.cwd, PathBuf::from("/"));
    }

    #[tokio::test]
    async fn test_cd_file_fails() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/file.txt".into()));

        let result = Cd.execute(args, &mut ctx).await;
        assert!(!result.ok());
        assert!(result.err.contains("Not a directory"));
    }

    #[tokio::test]
    async fn test_cd_nonexistent() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/nonexistent".into()));

        let result = Cd.execute(args, &mut ctx).await;
        assert!(!result.ok());
    }
}
