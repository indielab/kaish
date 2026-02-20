//! kaish-output-limit — Inspect and modify output size limit configuration.

use async_trait::async_trait;

use crate::interpreter::{ExecResult, OutputData, OutputNode};
use crate::output_limit::parse_size;
use crate::tools::{ExecContext, Tool, ToolArgs, ToolSchema};

/// Output limit tool: inspect and modify output size limit configuration.
pub struct KaishOutputLimit;

#[async_trait]
impl Tool for KaishOutputLimit {
    fn name(&self) -> &str {
        "kaish-output-limit"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("kaish-output-limit", "Inspect or modify output size limit configuration")
            .example("Show current config", "kaish-output-limit")
            .example("Set limit to 64KB", "kaish-output-limit set 64K")
            .example("Enable with default 64KB", "kaish-output-limit on")
            .example("Disable (unlimited)", "kaish-output-limit off")
            .example("Set head preview size", "kaish-output-limit head 2048")
            .example("Set tail preview size", "kaish-output-limit tail 1024")
    }

    async fn execute(&self, args: ToolArgs, ctx: &mut ExecContext) -> ExecResult {
        let subcommand = args.get_string("", 0);

        match subcommand.as_deref() {
            None | Some("") => show_config(ctx),
            Some("set") => {
                let size_str = match args.get_string("", 1) {
                    Some(s) => s,
                    None => return ExecResult::failure(1, "kaish-output-limit set: missing size (e.g., 64K, 1M, 65536)"),
                };
                match parse_size(&size_str) {
                    Ok(bytes) => {
                        ctx.output_limit.set_limit(Some(bytes));
                        show_config(ctx)
                    }
                    Err(e) => ExecResult::failure(1, format!("kaish-output-limit set: {}", e)),
                }
            }
            Some("on") => {
                if ctx.output_limit.max_bytes().is_none() {
                    ctx.output_limit.set_limit(Some(64 * 1024));
                }
                show_config(ctx)
            }
            Some("off") => {
                ctx.output_limit.set_limit(None);
                show_config(ctx)
            }
            Some("head") => {
                let size_str = match args.get_string("", 1) {
                    Some(s) => s,
                    None => return ExecResult::failure(1, "kaish-output-limit head: missing size"),
                };
                match parse_size(&size_str) {
                    Ok(bytes) => {
                        ctx.output_limit.set_head_bytes(bytes);
                        show_config(ctx)
                    }
                    Err(e) => ExecResult::failure(1, format!("kaish-output-limit head: {}", e)),
                }
            }
            Some("tail") => {
                let size_str = match args.get_string("", 1) {
                    Some(s) => s,
                    None => return ExecResult::failure(1, "kaish-output-limit tail: missing size"),
                };
                match parse_size(&size_str) {
                    Ok(bytes) => {
                        ctx.output_limit.set_tail_bytes(bytes);
                        show_config(ctx)
                    }
                    Err(e) => ExecResult::failure(1, format!("kaish-output-limit tail: {}", e)),
                }
            }
            Some(other) => ExecResult::failure(1, format!(
                "kaish-output-limit: unknown subcommand '{}' (try: set, on, off, head, tail)",
                other
            )),
        }
    }
}

fn show_config(ctx: &ExecContext) -> ExecResult {
    let cfg = &ctx.output_limit;
    let limit_str = match cfg.max_bytes() {
        Some(bytes) => format_size(bytes),
        None => "unlimited".to_string(),
    };

    let headers = vec!["KEY".to_string(), "VALUE".to_string()];
    let rows = vec![
        OutputNode::new("enabled").with_cells(vec![on_off(cfg.is_enabled())]),
        OutputNode::new("max-bytes").with_cells(vec![limit_str]),
        OutputNode::new("head-bytes").with_cells(vec![format_size(cfg.head_bytes())]),
        OutputNode::new("tail-bytes").with_cells(vec![format_size(cfg.tail_bytes())]),
        OutputNode::new("spill-dir").with_cells(vec![crate::paths::spill_dir().to_string_lossy().to_string()]),
    ];

    ExecResult::with_output(OutputData::table(headers, rows))
}

fn on_off(v: bool) -> String {
    if v { "on".to_string() } else { "off".to_string() }
}

fn format_size(bytes: usize) -> String {
    if bytes == 0 {
        "0".to_string()
    } else if bytes % (1024 * 1024) == 0 {
        format!("{}M", bytes / (1024 * 1024))
    } else if bytes % 1024 == 0 {
        format!("{}K", bytes / 1024)
    } else {
        bytes.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Value;
    use crate::vfs::{MemoryFs, VfsRouter};
    use std::sync::Arc;

    fn make_ctx() -> ExecContext {
        let mut vfs = VfsRouter::new();
        vfs.mount("/", MemoryFs::new());
        ExecContext::new(Arc::new(vfs))
    }

    #[tokio::test]
    async fn test_show_default() {
        let mut ctx = make_ctx();
        let args = ToolArgs::new();
        let result = KaishOutputLimit.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert!(result.out.contains("off"));
        assert!(result.out.contains("unlimited"));
    }

    #[tokio::test]
    async fn test_set_limit() {
        let mut ctx = make_ctx();
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("set".into()));
        args.positional.push(Value::String("64K".into()));
        let result = KaishOutputLimit.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert_eq!(ctx.output_limit.max_bytes(), Some(64 * 1024));
        assert!(result.out.contains("64K"));
    }

    #[tokio::test]
    async fn test_on_off() {
        let mut ctx = make_ctx();

        // Turn on
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("on".into()));
        let result = KaishOutputLimit.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert!(ctx.output_limit.is_enabled());

        // Turn off
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("off".into()));
        let result = KaishOutputLimit.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert!(!ctx.output_limit.is_enabled());
    }

    #[tokio::test]
    async fn test_head_tail() {
        let mut ctx = make_ctx();

        let mut args = ToolArgs::new();
        args.positional.push(Value::String("head".into()));
        args.positional.push(Value::String("2048".into()));
        let result = KaishOutputLimit.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert_eq!(ctx.output_limit.head_bytes(), 2048);

        let mut args = ToolArgs::new();
        args.positional.push(Value::String("tail".into()));
        args.positional.push(Value::String("1K".into()));
        let result = KaishOutputLimit.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert_eq!(ctx.output_limit.tail_bytes(), 1024);
    }

    #[tokio::test]
    async fn test_unknown_subcommand() {
        let mut ctx = make_ctx();
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("bogus".into()));
        let result = KaishOutputLimit.execute(args, &mut ctx).await;
        assert!(!result.ok());
        assert!(result.err.contains("unknown subcommand"));
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0");
        assert_eq!(format_size(1024), "1K");
        assert_eq!(format_size(64 * 1024), "64K");
        assert_eq!(format_size(1024 * 1024), "1M");
        assert_eq!(format_size(512), "512");
    }
}
