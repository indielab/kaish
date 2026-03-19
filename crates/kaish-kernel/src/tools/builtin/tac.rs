//! tac — Reverse lines of files or stdin.

use async_trait::async_trait;
use std::path::Path;

use crate::ast::Value;
use crate::interpreter::{ExecResult, OutputData};
use crate::tools::{ExecContext, ParamSchema, Tool, ToolArgs, ToolSchema};

/// Tac tool: output lines in reverse order.
pub struct Tac;

#[async_trait]
impl Tool for Tac {
    fn name(&self) -> &str {
        "tac"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("tac", "Reverse lines of files or stdin")
            .param(ParamSchema::optional(
                "path",
                "string",
                Value::Null,
                "File(s) to reverse (reads stdin if not provided)",
            ))
            .example("Reverse a file", "tac log.txt")
            .example("Reverse stdin", "seq 1 5 | tac")
    }

    async fn execute(&self, args: ToolArgs, ctx: &mut ExecContext) -> ExecResult {
        // Collect file paths, expanding globs
        let paths = match ctx.expand_paths(&args.positional).await {
            Ok(p) => p,
            Err(e) => return ExecResult::failure(1, format!("tac: {}", e)),
        };

        // Multiple files: reverse each in order (like GNU tac)
        if paths.len() > 1 {
            let mut output = String::new();
            for path in &paths {
                let resolved = ctx.resolve_path(path);
                match ctx.backend.read(Path::new(&resolved), None).await {
                    Ok(data) => match String::from_utf8(data) {
                        Ok(s) => {
                            let mut lines: Vec<&str> = s.lines().collect();
                            lines.reverse();
                            if !output.is_empty() {
                                output.push('\n');
                            }
                            output.push_str(&lines.join("\n"));
                        }
                        Err(_) => {
                            return ExecResult::failure(
                                1,
                                format!("tac: {}: invalid UTF-8", path),
                            )
                        }
                    },
                    Err(e) => return ExecResult::failure(1, format!("tac: {}: {}", path, e)),
                }
            }
            return ExecResult::with_output(OutputData::text(output));
        }

        // Single file or stdin
        let input = match paths.first() {
            Some(path) => {
                let resolved = ctx.resolve_path(path);
                match ctx.backend.read(Path::new(&resolved), None).await {
                    Ok(data) => match String::from_utf8(data) {
                        Ok(s) => s,
                        Err(_) => {
                            return ExecResult::failure(
                                1,
                                format!("tac: {}: invalid UTF-8", path),
                            )
                        }
                    },
                    Err(e) => return ExecResult::failure(1, format!("tac: {}: {}", path, e)),
                }
            }
            None => ctx.read_stdin_to_string().await.unwrap_or_default(),
        };

        let mut lines: Vec<&str> = input.lines().collect();
        lines.reverse();

        ExecResult::with_output(OutputData::text(lines.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::{Filesystem, MemoryFs, VfsRouter};
    use std::sync::Arc;

    async fn make_ctx() -> ExecContext {
        let mut vfs = VfsRouter::new();
        let mem = MemoryFs::new();
        mem.write(Path::new("lines.txt"), b"one\ntwo\nthree\nfour\nfive")
            .await
            .expect("write failed");
        mem.write(Path::new("single.txt"), b"only")
            .await
            .expect("write failed");
        mem.write(Path::new("a.txt"), b"a1\na2")
            .await
            .expect("write failed");
        mem.write(Path::new("b.txt"), b"b1\nb2")
            .await
            .expect("write failed");
        vfs.mount("/", mem);
        ExecContext::new(Arc::new(vfs))
    }

    #[tokio::test]
    async fn test_tac_file() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/lines.txt".into()));

        let result = Tac.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert_eq!(result.text_out().as_ref(), "five\nfour\nthree\ntwo\none");
    }

    #[tokio::test]
    async fn test_tac_stdin() {
        let mut ctx = make_ctx().await;
        ctx.set_stdin("alpha\nbeta\ngamma".to_string());

        let args = ToolArgs::new();
        let result = Tac.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert_eq!(result.text_out().as_ref(), "gamma\nbeta\nalpha");
    }

    #[tokio::test]
    async fn test_tac_single_line() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/single.txt".into()));

        let result = Tac.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert_eq!(result.text_out().as_ref(), "only");
    }

    #[tokio::test]
    async fn test_tac_empty_stdin() {
        let mut ctx = make_ctx().await;
        ctx.set_stdin("".to_string());

        let args = ToolArgs::new();
        let result = Tac.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert_eq!(result.text_out().as_ref(), "");
    }

    #[tokio::test]
    async fn test_tac_multiple_files() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/a.txt".into()));
        args.positional.push(Value::String("/b.txt".into()));

        let result = Tac.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert_eq!(result.text_out().as_ref(), "a2\na1\nb2\nb1");
    }

    #[tokio::test]
    async fn test_tac_file_not_found() {
        let mut ctx = make_ctx().await;
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("/nope.txt".into()));

        let result = Tac.execute(args, &mut ctx).await;
        assert!(!result.ok());
    }
}
