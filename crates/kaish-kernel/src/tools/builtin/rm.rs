//! rm — Remove files and directories.

use async_trait::async_trait;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::ast::Value;
use crate::interpreter::ExecResult;
use crate::tools::{ExecContext, Tool, ToolArgs, ToolSchema, ParamSchema};
use crate::vfs::{EntryType, Filesystem};

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
            .param(ParamSchema::optional(
                "recursive",
                "bool",
                Value::Bool(false),
                "Remove directories and their contents recursively (-r)",
            ))
            .param(ParamSchema::optional(
                "force",
                "bool",
                Value::Bool(false),
                "Ignore nonexistent files, never prompt (-f)",
            ))
    }

    async fn execute(&self, args: ToolArgs, ctx: &mut ExecContext) -> ExecResult {
        let path = match args.get_string("path", 0) {
            Some(p) => p,
            None => return ExecResult::failure(1, "rm: missing path argument"),
        };

        let recursive = args.has_flag("recursive") || args.has_flag("r");
        let force = args.has_flag("force") || args.has_flag("f");
        let resolved = ctx.resolve_path(&path);

        match remove_path(ctx, Path::new(&resolved), recursive, force).await {
            Ok(()) => ExecResult::success(""),
            Err(e) => ExecResult::failure(1, format!("rm: {}: {}", path, e)),
        }
    }
}

/// Remove a path, optionally recursively.
async fn remove_path(ctx: &ExecContext, path: &Path, recursive: bool, force: bool) -> std::io::Result<()> {
    // Check if path exists
    match ctx.vfs.stat(path).await {
        Ok(meta) => {
            if meta.is_dir && recursive {
                // Remove contents first
                remove_dir_recursive(ctx, path).await?;
            }
            ctx.vfs.remove(path).await
        }
        Err(e) if e.kind() == ErrorKind::NotFound && force => {
            // -f ignores nonexistent files
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Recursively remove directory contents, then the directory itself.
fn remove_dir_recursive<'a>(
    ctx: &'a ExecContext,
    dir: &'a Path,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send + 'a>> {
    Box::pin(async move {
        let entries = ctx.vfs.list(dir).await?;

        for entry in entries {
            let child_path: PathBuf = dir.join(&entry.name);
            match entry.entry_type {
                EntryType::Directory => {
                    // Recurse into subdirectory
                    remove_dir_recursive(ctx, &child_path).await?;
                    ctx.vfs.remove(&child_path).await?;
                }
                EntryType::File => {
                    ctx.vfs.remove(&child_path).await?;
                }
            }
        }

        Ok(())
    })
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

    #[tokio::test]
    async fn test_rm_r_recursive() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/fulldir".into()));
        args.flags.insert("r".to_string());

        let result = Rm.execute(args, &mut ctx).await;
        assert!(result.ok());

        // Verify directory and contents removed
        assert!(!ctx.vfs.exists(Path::new("/fulldir")).await);
        assert!(!ctx.vfs.exists(Path::new("/fulldir/file.txt")).await);
    }

    #[tokio::test]
    async fn test_rm_recursive_flag() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/fulldir".into()));
        args.flags.insert("recursive".to_string());

        let result = Rm.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert!(!ctx.vfs.exists(Path::new("/fulldir")).await);
    }

    #[tokio::test]
    async fn test_rm_f_force_nonexistent() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/nonexistent".into()));
        args.flags.insert("f".to_string());

        let result = Rm.execute(args, &mut ctx).await;
        assert!(result.ok()); // -f silences not-found errors
    }

    #[tokio::test]
    async fn test_rm_force_flag_nonexistent() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/nonexistent".into()));
        args.flags.insert("force".to_string());

        let result = Rm.execute(args, &mut ctx).await;
        assert!(result.ok());
    }

    async fn make_deep_ctx() -> ExecContext {
        let mut vfs = VfsRouter::new();
        let mem = MemoryFs::new();
        mem.write(Path::new("deep/a/b/c/file.txt"), b"data").await.unwrap();
        mem.write(Path::new("deep/a/sibling.txt"), b"data").await.unwrap();
        vfs.mount("/", mem);
        ExecContext::new(Arc::new(vfs))
    }

    #[tokio::test]
    async fn test_rm_r_deeply_nested() {
        let mut ctx = make_deep_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/deep".into()));
        args.flags.insert("r".to_string());

        let result = Rm.execute(args, &mut ctx).await;
        assert!(result.ok());

        assert!(!ctx.vfs.exists(Path::new("/deep")).await);
        assert!(!ctx.vfs.exists(Path::new("/deep/a")).await);
        assert!(!ctx.vfs.exists(Path::new("/deep/a/b")).await);
    }
}
