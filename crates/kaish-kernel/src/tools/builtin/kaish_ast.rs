//! kaish-ast — Parse and display AST without executing.
//!
//! # Examples
//!
//! ```kaish
//! kaish-ast 'echo hello | grep h'   # One-shot: print AST
//! kaish-ast -on                      # Toggle AST mode on
//! kaish-ast -off                     # Toggle AST mode off
//! ```

use async_trait::async_trait;

use crate::ast::Value;
use crate::interpreter::{ExecResult, OutputData};
use crate::parser::parse;
use crate::tools::{ExecContext, ParamSchema, Tool, ToolArgs, ToolSchema};

/// kaish-ast: parse expressions and display their AST.
pub struct KaishAst;

#[async_trait]
impl Tool for KaishAst {
    fn name(&self) -> &str {
        "kaish-ast"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("kaish-ast", "Parse and display AST without executing")
            .param(ParamSchema::optional(
                "expr",
                "string",
                Value::Null,
                "Expression to parse and display",
            ))
            .param(ParamSchema::optional(
                "on",
                "bool",
                Value::Bool(false),
                "Enable AST mode (show AST for every command)",
            ))
            .param(ParamSchema::optional(
                "off",
                "bool",
                Value::Bool(false),
                "Disable AST mode",
            ))
            .example("Parse an expression", "kaish-ast 'echo hello | grep h'")
            .example("Enable AST mode", "kaish-ast -on")
            .example("Disable AST mode", "kaish-ast -off")
    }

    async fn execute(&self, args: ToolArgs, ctx: &mut ExecContext) -> ExecResult {
        // Toggle mode
        if args.has_flag("on") {
            ctx.scope.set_show_ast(true);
            return ExecResult::with_output(OutputData::text("AST mode: ON\n"));
        }
        if args.has_flag("off") {
            ctx.scope.set_show_ast(false);
            return ExecResult::with_output(OutputData::text("AST mode: OFF\n"));
        }

        // One-shot: parse expression and display AST
        let expr = match args.get_string("expr", 0) {
            Some(e) => e,
            None => {
                // Toggle mode if no args
                let current = ctx.scope.show_ast();
                ctx.scope.set_show_ast(!current);
                let state = if !current { "ON" } else { "OFF" };
                return ExecResult::with_output(OutputData::text(format!("AST mode: {state}\n")));
            }
        };

        match parse(&expr) {
            Ok(program) => ExecResult::with_output(OutputData::text(format!("{:#?}\n", program))),
            Err(errors) => {
                let mut msg = String::from("Parse error:\n");
                for err in errors {
                    msg.push_str(&format!("  {err}\n"));
                }
                ExecResult::failure(1, msg)
            }
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
    async fn test_ast_one_shot() {
        let mut ctx = make_ctx();
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("echo hello".into()));

        let result = KaishAst.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert!(result.out.contains("Echo") || result.out.contains("Command") || result.out.contains("echo"));
    }

    #[tokio::test]
    async fn test_ast_toggle_on() {
        let mut ctx = make_ctx();
        let mut args = ToolArgs::new();
        args.flags.insert("on".to_string());

        let result = KaishAst.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert!(result.out.contains("ON"));
        assert!(ctx.scope.show_ast());
    }

    #[tokio::test]
    async fn test_ast_toggle_off() {
        let mut ctx = make_ctx();
        ctx.scope.set_show_ast(true);
        let mut args = ToolArgs::new();
        args.flags.insert("off".to_string());

        let result = KaishAst.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert!(result.out.contains("OFF"));
        assert!(!ctx.scope.show_ast());
    }

    #[tokio::test]
    async fn test_ast_parse_error() {
        let mut ctx = make_ctx();
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("if".into()));

        let result = KaishAst.execute(args, &mut ctx).await;
        assert!(!result.ok());
    }
}
