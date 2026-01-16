//! kaish REPL — Interactive shell for 会sh.
//!
//! This is an evolving REPL that grows with each layer of the kaish project.
//! Currently (L6), it provides:
//!
//! - Parse input and display AST (`/ast` toggle)
//! - Evaluate expressions with persistent Scope
//! - `set X = value` assignments
//! - Real tool execution via VFS (ls, cat, echo, cd, pwd, mkdir, write, rm)
//! - Meta-commands: `/help`, `/quit`, `/ast`, `/scope`, `/cwd`

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline::Editor;
use tokio::runtime::Runtime;

use kaish_kernel::ast::{Arg, Expr, Pipeline, Stmt, Value};
use kaish_kernel::interpreter::{ExecResult, Scope};
use kaish_kernel::parser::parse;
use kaish_kernel::tools::{ExecContext, ToolArgs, ToolRegistry, register_builtins};
use kaish_kernel::vfs::{LocalFs, MemoryFs, VfsRouter};

/// REPL configuration and state.
pub struct Repl {
    scope: Scope,
    show_ast: bool,
    tools: ToolRegistry,
    exec_ctx: ExecContext,
    runtime: Runtime,
}

impl Repl {
    /// Create a new REPL instance with VFS rooted at current directory.
    pub fn new() -> Result<Self> {
        let cwd = std::env::current_dir().context("Failed to get current directory")?;
        Self::with_root(cwd)
    }

    /// Create a new REPL with VFS rooted at the given path.
    pub fn with_root(root: PathBuf) -> Result<Self> {
        // Build the VFS
        let mut vfs = VfsRouter::new();

        // Mount the real filesystem at /mnt/local
        let local_fs = LocalFs::new(root.clone());
        vfs.mount("/mnt/local", local_fs);

        // Mount a memory fs at /scratch for ephemeral data
        vfs.mount("/scratch", MemoryFs::new());

        // Mount root as memory fs (for now)
        vfs.mount("/", MemoryFs::new());

        // Create execution context starting at /mnt/local
        let mut exec_ctx = ExecContext::new(Arc::new(vfs));
        exec_ctx.set_cwd(PathBuf::from("/mnt/local"));

        // Build tool registry with builtins
        let mut tools = ToolRegistry::new();
        register_builtins(&mut tools);

        // Create tokio runtime for async tool execution
        let runtime = Runtime::new().context("Failed to create tokio runtime")?;

        Ok(Self {
            scope: Scope::new(),
            show_ast: false,
            tools,
            exec_ctx,
            runtime,
        })
    }

    /// Process a single line of input.
    pub fn process_line(&mut self, line: &str) -> Result<Option<String>> {
        let trimmed = line.trim();

        // Handle meta-commands
        if trimmed.starts_with('/') {
            return self.handle_meta_command(trimmed);
        }

        // Skip empty lines
        if trimmed.is_empty() {
            return Ok(None);
        }

        // Parse the input
        let program = match parse(trimmed) {
            Ok(prog) => prog,
            Err(errors) => {
                let mut msg = String::from("Parse error:\n");
                for err in errors {
                    msg.push_str(&format!("  {err}\n"));
                }
                return Ok(Some(msg));
            }
        };

        // Show AST if enabled
        if self.show_ast {
            return Ok(Some(format!("{:#?}", program)));
        }

        // Execute each statement
        let mut output = String::new();
        for stmt in program.statements {
            if let Some(result) = self.execute_stmt(&stmt)? {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(&result);
            }
        }

        if output.is_empty() {
            Ok(None)
        } else {
            Ok(Some(output))
        }
    }

    /// Execute a single statement.
    fn execute_stmt(&mut self, stmt: &Stmt) -> Result<Option<String>> {
        match stmt {
            Stmt::Assignment(assign) => {
                let value = self.eval_expr(&assign.value)?;
                self.scope.set(&assign.name, value.clone());
                Ok(Some(format!("{} = {}", assign.name, format_value(&value))))
            }
            Stmt::Command(cmd) => {
                let result = self.execute_command(&cmd.name, &cmd.args)?;
                self.scope.set_last_result(result.clone());
                Ok(Some(format_result(&result)))
            }
            Stmt::Pipeline(pipeline) => {
                let result = self.execute_pipeline(pipeline)?;
                self.scope.set_last_result(result.clone());
                Ok(Some(format_result(&result)))
            }
            Stmt::If(if_stmt) => {
                let cond_value = self.eval_expr(&if_stmt.condition)?;
                let branch = if is_truthy(&cond_value) {
                    &if_stmt.then_branch
                } else {
                    if_stmt.else_branch.as_ref().map(|v| v.as_slice()).unwrap_or(&[])
                };

                let mut output = String::new();
                for stmt in branch {
                    if let Some(result) = self.execute_stmt(stmt)? {
                        if !output.is_empty() {
                            output.push('\n');
                        }
                        output.push_str(&result);
                    }
                }
                Ok(if output.is_empty() { None } else { Some(output) })
            }
            Stmt::For(for_loop) => {
                let iterable = self.eval_expr(&for_loop.iterable)?;
                let items = match iterable {
                    Value::Array(items) => items,
                    _ => return Ok(Some("Error: for loop requires an array".into())),
                };

                self.scope.push_frame();
                let mut output = String::new();

                for item in items {
                    if let Expr::Literal(value) = item {
                        self.scope.set(&for_loop.variable, value);
                        for stmt in &for_loop.body {
                            if let Some(result) = self.execute_stmt(stmt)? {
                                if !output.is_empty() {
                                    output.push('\n');
                                }
                                output.push_str(&result);
                            }
                        }
                    }
                }

                self.scope.pop_frame();
                Ok(if output.is_empty() { None } else { Some(output) })
            }
            Stmt::ToolDef(tool) => {
                Ok(Some(format!("Defined tool: {}", tool.name)))
            }
            Stmt::Empty => Ok(None),
        }
    }

    /// Execute a command using the tool registry.
    fn execute_command(&mut self, name: &str, args: &[Arg]) -> Result<ExecResult> {
        // Special built-ins that don't need the tool registry
        match name {
            "true" => return Ok(ExecResult::success("")),
            "false" => return Ok(ExecResult::failure(1, "")),
            _ => {}
        }

        // Look up tool in registry
        let tool = match self.tools.get(name) {
            Some(t) => t,
            None => {
                return Ok(ExecResult::failure(
                    127,
                    format!("{}: command not found", name),
                ));
            }
        };

        // Convert AST args to ToolArgs
        let mut tool_args = ToolArgs::new();
        for arg in args {
            match arg {
                Arg::Positional(expr) => {
                    let value = self.eval_expr(expr)?;
                    tool_args.positional.push(value);
                }
                Arg::Named { key, value } => {
                    let val = self.eval_expr(value)?;
                    tool_args.named.insert(key.clone(), val);
                }
            }
        }

        // Execute the tool asynchronously
        let result = self.runtime.block_on(tool.execute(tool_args, &mut self.exec_ctx));

        // Sync cwd back to scope if cd was called
        if name == "cd" && result.ok() {
            // Update scope with new cwd for display
            self.scope.set("CWD", Value::String(
                self.exec_ctx.cwd.to_string_lossy().to_string()
            ));
        }

        Ok(result)
    }

    /// Execute a pipeline (stub implementation).
    fn execute_pipeline(&mut self, pipeline: &Pipeline) -> Result<ExecResult> {
        if pipeline.commands.len() == 1 {
            // Single command, just execute it
            let cmd = &pipeline.commands[0];
            let mut result = self.execute_command(&cmd.name, &cmd.args)?;
            if pipeline.background {
                result = ExecResult::success(format!("[bg] {}", result.out));
            }
            return Ok(result);
        }

        // Multi-command pipeline: stub
        let cmd_names: Vec<_> = pipeline.commands.iter().map(|c| c.name.as_str()).collect();
        let pipeline_str = cmd_names.join(" | ");

        if pipeline.background {
            Ok(ExecResult::success(format!("[stub] {} &", pipeline_str)))
        } else {
            Ok(ExecResult::success(format!("[stub pipeline] {}", pipeline_str)))
        }
    }

    /// Evaluate an expression using the scope.
    fn eval_expr(&mut self, expr: &Expr) -> Result<Value> {
        // Simple evaluation without the full Evaluator (avoids borrow issues)
        // Command substitution will be stubbed
        self.eval_expr_inner(expr)
    }

    fn eval_expr_inner(&mut self, expr: &Expr) -> Result<Value> {
        match expr {
            Expr::Literal(value) => self.eval_literal(value),
            Expr::VarRef(path) => {
                self.scope.resolve_path(path)
                    .ok_or_else(|| anyhow::anyhow!("undefined variable"))
            }
            Expr::Interpolated(parts) => {
                let mut result = String::new();
                for part in parts {
                    match part {
                        kaish_kernel::ast::StringPart::Literal(s) => result.push_str(s),
                        kaish_kernel::ast::StringPart::Var(path) => {
                            let value = self.scope.resolve_path(path)
                                .ok_or_else(|| anyhow::anyhow!("undefined variable in interpolation"))?;
                            result.push_str(&format_value_unquoted(&value));
                        }
                    }
                }
                Ok(Value::String(result))
            }
            Expr::BinaryOp { left, op, right } => {
                use kaish_kernel::ast::BinaryOp;
                match op {
                    BinaryOp::And => {
                        let left_val = self.eval_expr_inner(left)?;
                        if !is_truthy(&left_val) {
                            return Ok(left_val);
                        }
                        self.eval_expr_inner(right)
                    }
                    BinaryOp::Or => {
                        let left_val = self.eval_expr_inner(left)?;
                        if is_truthy(&left_val) {
                            return Ok(left_val);
                        }
                        self.eval_expr_inner(right)
                    }
                    BinaryOp::Eq => {
                        let l = self.eval_expr_inner(left)?;
                        let r = self.eval_expr_inner(right)?;
                        Ok(Value::Bool(values_equal(&l, &r)))
                    }
                    BinaryOp::NotEq => {
                        let l = self.eval_expr_inner(left)?;
                        let r = self.eval_expr_inner(right)?;
                        Ok(Value::Bool(!values_equal(&l, &r)))
                    }
                    BinaryOp::Lt | BinaryOp::Gt | BinaryOp::LtEq | BinaryOp::GtEq => {
                        let l = self.eval_expr_inner(left)?;
                        let r = self.eval_expr_inner(right)?;
                        let ord = compare_values(&l, &r)?;
                        let result = match op {
                            BinaryOp::Lt => ord.is_lt(),
                            BinaryOp::Gt => ord.is_gt(),
                            BinaryOp::LtEq => ord.is_le(),
                            BinaryOp::GtEq => ord.is_ge(),
                            _ => unreachable!(),
                        };
                        Ok(Value::Bool(result))
                    }
                }
            }
            Expr::CommandSubst(pipeline) => {
                // Execute the command and return its result as an object
                let result = self.execute_pipeline(pipeline)?;
                self.scope.set_last_result(result.clone());
                Ok(result_to_value(&result))
            }
        }
    }

    fn eval_literal(&mut self, value: &Value) -> Result<Value> {
        match value {
            Value::Array(items) => {
                let evaluated: Result<Vec<_>> = items
                    .iter()
                    .map(|expr| self.eval_expr_inner(expr).map(|v| Expr::Literal(v)))
                    .collect();
                Ok(Value::Array(evaluated?))
            }
            Value::Object(fields) => {
                let evaluated: Result<Vec<_>> = fields
                    .iter()
                    .map(|(k, expr)| self.eval_expr_inner(expr).map(|v| (k.clone(), Expr::Literal(v))))
                    .collect();
                Ok(Value::Object(evaluated?))
            }
            _ => Ok(value.clone()),
        }
    }

    /// Handle a meta-command (starts with /).
    fn handle_meta_command(&mut self, cmd: &str) -> Result<Option<String>> {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        let command = parts.first().copied().unwrap_or("");

        match command {
            "/quit" | "/q" | "/exit" => {
                std::process::exit(0);
            }
            "/help" | "/h" | "/?" => {
                Ok(Some(HELP_TEXT.to_string()))
            }
            "/ast" => {
                self.show_ast = !self.show_ast;
                Ok(Some(format!("AST mode: {}", if self.show_ast { "ON" } else { "OFF" })))
            }
            "/scope" | "/vars" => {
                let names = self.scope.all_names();
                if names.is_empty() {
                    Ok(Some("(no variables set)".to_string()))
                } else {
                    let mut output = String::from("Variables:\n");
                    for name in names {
                        if let Some(value) = self.scope.get(name) {
                            output.push_str(&format!("  {} = {}\n", name, format_value(value)));
                        }
                    }
                    Ok(Some(output.trim_end().to_string()))
                }
            }
            "/result" | "/$?" => {
                let result = self.scope.last_result();
                Ok(Some(format_result(result)))
            }
            "/cwd" => {
                Ok(Some(self.exec_ctx.cwd.to_string_lossy().to_string()))
            }
            "/tools" => {
                let names = self.tools.names();
                Ok(Some(format!("Available tools: {}", names.join(", "))))
            }
            _ => {
                Ok(Some(format!("Unknown command: {}\nType /help for available commands.", command)))
            }
        }
    }
}

impl Default for Repl {
    fn default() -> Self {
        Self::new().expect("Failed to create REPL")
    }
}

/// Format a Value for display (with quotes on strings).
fn format_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::String(s) => format!("\"{}\"", s),
        Value::Array(items) => {
            let formatted: Vec<String> = items
                .iter()
                .filter_map(|e| {
                    if let Expr::Literal(v) = e {
                        Some(format_value(v))
                    } else {
                        Some("<expr>".to_string())
                    }
                })
                .collect();
            format!("[{}]", formatted.join(", "))
        }
        Value::Object(fields) => {
            let formatted: Vec<String> = fields
                .iter()
                .map(|(k, e)| {
                    let v = if let Expr::Literal(v) = e {
                        format_value(v)
                    } else {
                        "<expr>".to_string()
                    };
                    format!("\"{}\": {}", k, v)
                })
                .collect();
            format!("{{{}}}", formatted.join(", "))
        }
    }
}

/// Format a Value for display (without quotes on strings, for echo).
fn format_value_unquoted(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        _ => format_value(value),
    }
}

/// Format an ExecResult for display.
fn format_result(result: &ExecResult) -> String {
    let status = if result.ok() { "✓" } else { "✗" };
    let mut output = format!("{} code={}", status, result.code);

    if !result.out.is_empty() {
        if result.out.contains('\n') {
            output.push_str(&format!("\n{}", result.out));
        } else {
            output.push_str(&format!(" out={}", result.out));
        }
    }

    if !result.err.is_empty() {
        output.push_str(&format!(" err=\"{}\"", result.err));
    }

    output
}

/// Check if a value is truthy.
fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Int(i) => *i != 0,
        Value::Float(f) => *f != 0.0,
        Value::String(s) => !s.is_empty(),
        Value::Array(arr) => !arr.is_empty(),
        Value::Object(_) => true,
    }
}

/// Check if two values are equal.
fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => (a - b).abs() < f64::EPSILON,
        (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => {
            (*a as f64 - b).abs() < f64::EPSILON
        }
        (Value::String(a), Value::String(b)) => a == b,
        _ => false,
    }
}

/// Compare two values for ordering.
fn compare_values(left: &Value, right: &Value) -> Result<std::cmp::Ordering> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b)),
        (Value::Float(a), Value::Float(b)) => {
            a.partial_cmp(b).ok_or_else(|| anyhow::anyhow!("NaN comparison"))
        }
        (Value::Int(a), Value::Float(b)) => {
            (*a as f64).partial_cmp(b).ok_or_else(|| anyhow::anyhow!("NaN comparison"))
        }
        (Value::Float(a), Value::Int(b)) => {
            a.partial_cmp(&(*b as f64)).ok_or_else(|| anyhow::anyhow!("NaN comparison"))
        }
        (Value::String(a), Value::String(b)) => Ok(a.cmp(b)),
        _ => Err(anyhow::anyhow!("cannot compare these types")),
    }
}

/// Convert an ExecResult to a Value.
fn result_to_value(result: &ExecResult) -> Value {
    let mut fields = vec![
        ("code".into(), Expr::Literal(Value::Int(result.code))),
        ("ok".into(), Expr::Literal(Value::Bool(result.ok()))),
        ("out".into(), Expr::Literal(Value::String(result.out.clone()))),
        ("err".into(), Expr::Literal(Value::String(result.err.clone()))),
    ];
    if let Some(data) = &result.data {
        fields.push(("data".into(), Expr::Literal(data.clone())));
    }
    Value::Object(fields)
}

const HELP_TEXT: &str = r#"会sh — kaish REPL (Layer 6)

Meta Commands:
  /help, /h, /?     Show this help
  /quit, /q, /exit  Exit the REPL
  /ast              Toggle AST display mode
  /scope, /vars     Show all variables
  /result, /$?      Show last command result
  /cwd              Show current working directory
  /tools            List available tools

Built-in Tools:
  echo [args...]    Print arguments
  pwd               Print working directory
  cd [path]         Change directory
  ls [path] [-l]    List directory contents
  cat <path>        Read file contents
  mkdir <path>      Create directory
  write <path> <content>  Write to file
  rm <path>         Remove file or empty directory

Language:
  set X = value     Assign a variable
  ${VAR}            Variable reference
  ${VAR.field}      Nested access
  ${?.ok}           Last result access
  a | b             Pipeline (stub)
  if cond; then ... fi
  for X in arr; do ... done

Examples:
  ls                         # List current directory
  cd subdir                  # Change to subdir
  cat README.md              # Read a file
  echo "Hello ${USER}"       # Print with variable
  set DATA = {"count": 42}   # Create object
  echo ${DATA.count}         # Access field
"#;

/// Run the REPL.
pub fn run() -> Result<()> {
    println!("会sh — kaish v{} (Layer 6: Tools)", env!("CARGO_PKG_VERSION"));
    println!("Type /help for commands, /quit to exit.\n");

    let mut rl: Editor<(), DefaultHistory> = Editor::new()
        .context("Failed to create editor")?;

    // Load history if it exists
    let history_path = dirs::data_dir()
        .map(|p| p.join("kaish").join("history.txt"));
    if let Some(ref path) = history_path {
        let _ = rl.load_history(path);
    }

    let mut repl = Repl::new()?;

    loop {
        let prompt = "会sh> ";

        match rl.readline(prompt) {
            Ok(line) => {
                let _ = rl.add_history_entry(line.as_str());

                match repl.process_line(&line) {
                    Ok(Some(output)) => println!("{}", output),
                    Ok(None) => {}
                    Err(e) => eprintln!("Error: {}", e),
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("^C");
                continue;
            }
            Err(ReadlineError::Eof) => {
                println!("^D");
                break;
            }
            Err(err) => {
                eprintln!("Error: {}", err);
                break;
            }
        }
    }

    // Save history
    if let Some(ref path) = history_path {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = rl.save_history(path);
    }

    Ok(())
}
