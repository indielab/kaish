//! cat — Read and output file contents.

use async_trait::async_trait;
use std::path::Path;

use crate::interpreter::ExecResult;
use crate::tools::{ExecContext, Tool, ToolArgs, ToolSchema, ParamSchema};
use crate::vfs::Filesystem;

/// Cat tool: read and output file contents.
pub struct Cat;

#[async_trait]
impl Tool for Cat {
    fn name(&self) -> &str {
        "cat"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("cat", "Read and output file contents")
            .param(ParamSchema::required("path", "string", "File path to read"))
    }

    async fn execute(&self, args: ToolArgs, ctx: &mut ExecContext) -> ExecResult {
        let path = match args.get_string("path", 0) {
            Some(p) => p,
            None => return ExecResult::failure(1, "cat: missing path argument"),
        };

        let resolved = ctx.resolve_path(&path);

        match ctx.vfs.read(Path::new(&resolved)).await {
            Ok(data) => match String::from_utf8(data) {
                Ok(content) => ExecResult::success(content),
                Err(_) => ExecResult::failure(1, "cat: file contains invalid UTF-8"),
            },
            Err(e) => ExecResult::failure(1, format!("cat: {}: {}", path, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Value;
    use crate::vfs::{MemoryFs, VfsRouter};
    use std::sync::Arc;

    async fn make_ctx() -> ExecContext {
        let mut vfs = VfsRouter::new();
        let mem = MemoryFs::new();
        mem.write(Path::new("test.txt"), b"hello world").await.unwrap();
        mem.write(Path::new("dir/nested.txt"), b"nested content").await.unwrap();
        vfs.mount("/", mem);
        ExecContext::new(Arc::new(vfs))
    }

    #[tokio::test]
    async fn test_cat_file() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/test.txt".into()));

        let result = Cat.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert_eq!(result.out, "hello world");
    }

    #[tokio::test]
    async fn test_cat_nested() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/dir/nested.txt".into()));

        let result = Cat.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert_eq!(result.out, "nested content");
    }

    #[tokio::test]
    async fn test_cat_not_found() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/nonexistent.txt".into()));

        let result = Cat.execute(args, &mut ctx).await;
        assert!(!result.ok());
        assert!(result.err.contains("not found") || result.err.contains("nonexistent"));
    }

    #[tokio::test]
    async fn test_cat_no_arg() {
        let mut ctx = make_ctx().await;
        let args = ToolArgs::new();

        let result = Cat.execute(args, &mut ctx).await;
        assert!(!result.ok());
        assert!(result.err.contains("missing"));
    }
}
