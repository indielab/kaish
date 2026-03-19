//! timeout — Run a command with a time limit.
//!
//! Dispatches through the backend for builtins, spawns a child process
//! for external commands. Returns exit code 124 on timeout (matching
//! coreutils convention).

use async_trait::async_trait;
use std::time::Duration;
use tokio::process::Command;

use crate::ast::Value;
use crate::backend::BackendError;
use crate::interpreter::ExecResult;
use crate::tools::{ExecContext, ParamSchema, Tool, ToolArgs, ToolSchema};

use super::spawn::resolve_in_path;

/// Timeout tool: run a command with a deadline.
pub struct Timeout;

#[async_trait]
impl Tool for Timeout {
    fn name(&self) -> &str {
        "timeout"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("timeout", "Run a command with a time limit")
            .param(ParamSchema::required(
                "duration",
                "string",
                "Time limit: 30 (seconds), 30s, 500ms, 5m, 1h",
            ))
            .param(ParamSchema::required(
                "command",
                "string",
                "Command to run",
            ))
            .example("With seconds", "timeout 5 sleep 10")
            .example("With duration suffix", "timeout 500ms curl example.com")
            .example("Minutes", "timeout 2m cargo build")
    }

    async fn execute(&self, args: ToolArgs, ctx: &mut ExecContext) -> ExecResult {
        // Need at least duration + command
        if args.positional.len() < 2 {
            return ExecResult::failure(
                1,
                "timeout: usage: timeout DURATION COMMAND [ARGS...]",
            );
        }

        // Parse duration from first positional
        let duration_str = match &args.positional[0] {
            Value::String(s) => s.clone(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            other => {
                return ExecResult::failure(
                    1,
                    format!("timeout: invalid duration: {:?}", other),
                )
            }
        };

        let duration = match parse_duration(&duration_str) {
            Some(d) => d,
            None => {
                return ExecResult::failure(
                    1,
                    format!("timeout: invalid duration '{}' (try: 30, 5s, 500ms, 2m, 1h)", duration_str),
                )
            }
        };

        // Command name from second positional
        let cmd_name = match &args.positional[1] {
            Value::String(s) => s.clone(),
            other => {
                return ExecResult::failure(
                    1,
                    format!("timeout: invalid command: {:?}", other),
                )
            }
        };

        // Remaining positionals become the inner command's args
        let inner_args: Vec<Value> = args.positional[2..].to_vec();

        // Try builtin dispatch first via backend
        let mut tool_args = ToolArgs::new();
        tool_args.positional = inner_args.clone();

        let backend = ctx.backend.clone();
        let builtin_future = backend.call_tool(&cmd_name, tool_args, ctx);

        match tokio::time::timeout(duration, builtin_future).await {
            Ok(Ok(tool_result)) => {
                // Builtin succeeded within time limit
                let mut result = ExecResult::from_output(
                    tool_result.code as i64,
                    tool_result.stdout,
                    tool_result.stderr,
                );
                result.output = tool_result.output;
                result.content_type = tool_result.content_type;
                result.baggage = tool_result.baggage;
                if let Some(json_data) = tool_result.data {
                    result.data = Some(Value::Json(json_data));
                }
                result
            }
            Ok(Err(BackendError::ToolNotFound(_))) => {
                // Not a builtin — try external command
                self.run_external(ctx, &cmd_name, &inner_args, duration).await
            }
            Ok(Err(e)) => ExecResult::failure(1, format!("timeout: {}", e)),
            Err(_elapsed) => {
                // Builtin timed out
                ExecResult::failure(124, format!("timeout: timed out after {}", duration_str))
            }
        }
    }
}

impl Timeout {
    /// Run an external command with a timeout.
    async fn run_external(
        &self,
        ctx: &mut ExecContext,
        cmd_name: &str,
        inner_args: &[Value],
        duration: Duration,
    ) -> ExecResult {
        if !ctx.allow_external_commands {
            return ExecResult::failure(
                1,
                "timeout: external commands are disabled",
            );
        }

        // Resolve command in PATH
        let command = if cmd_name.starts_with('/') || cmd_name.starts_with("./") {
            cmd_name.to_string()
        } else {
            let path_var = ctx
                .scope
                .get("PATH")
                .map(value_to_string)
                .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default());

            match resolve_in_path(cmd_name, &path_var) {
                Some(resolved) => resolved,
                None => {
                    return ExecResult::failure(
                        127,
                        format!("timeout: {}: command not found", cmd_name),
                    )
                }
            }
        };

        // Convert args to strings
        let argv: Vec<String> = inner_args.iter().map(value_to_string).collect();

        // Build command
        let mut cmd = Command::new(&command);
        cmd.args(&argv);

        // Resolve cwd for external command
        let vfs_cwd = ctx.cwd.clone();
        if let Some(real_cwd) = ctx.backend.resolve_real_path(&vfs_cwd) {
            cmd.current_dir(&real_cwd);
        }

        // Handle stdin
        let stdin_data = ctx.read_stdin_to_string().await;
        cmd.stdin(if stdin_data.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        });
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // Spawn
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                return ExecResult::failure(127, format!("timeout: {}: {}", command, e))
            }
        };

        // Write stdin if present
        if let Some(data) = stdin_data {
            if let Some(mut stdin) = child.stdin.take() {
                use tokio::io::AsyncWriteExt;
                // Ignore stdin write errors (broken pipe is fine)
                let _ = stdin.write_all(data.as_bytes()).await;
            }
        }

        // Take stdout/stderr handles so we can read them while keeping child alive for kill
        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        // Spawn readers for stdout and stderr
        let stdout_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = Vec::new();
            if let Some(mut out) = stdout_handle {
                let _ = out.read_to_end(&mut buf).await;
            }
            buf
        });
        let stderr_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = Vec::new();
            if let Some(mut err) = stderr_handle {
                let _ = err.read_to_end(&mut buf).await;
            }
            buf
        });

        // Wait with timeout — we keep &mut child so we can kill on timeout
        match tokio::time::timeout(duration, child.wait()).await {
            Ok(Ok(status)) => {
                let code = status.code().unwrap_or(-1) as i64;
                let stdout_bytes = stdout_task.await.unwrap_or_default();
                let stderr_bytes = stderr_task.await.unwrap_or_default();
                let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
                let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
                ExecResult::from_output(code, stdout, stderr)
            }
            Ok(Err(e)) => ExecResult::failure(1, format!("timeout: {}: {}", command, e)),
            Err(_elapsed) => {
                // Kill the child process, then collect any partial output
                let _ = child.kill().await;
                let stdout_bytes = stdout_task.await.unwrap_or_default();
                let stderr_bytes = stderr_task.await.unwrap_or_default();
                let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
                let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
                let mut result = ExecResult::from_output(124, stdout, stderr);
                // Append timeout message to stderr
                if !result.err.is_empty() && !result.err.ends_with('\n') {
                    result.err.push('\n');
                }
                result.err.push_str(&format!("timeout: {}: timed out", cmd_name));
                result
            }
        }
    }
}

/// Parse a duration string: "30" (seconds), "30s", "500ms", "5m", "1h"
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();

    // Try pure number (seconds)
    if let Ok(secs) = s.parse::<f64>() {
        return if secs >= 0.0 {
            Some(Duration::from_secs_f64(secs))
        } else {
            None
        };
    }

    // Try with suffix
    if let Some(num) = s.strip_suffix("ms") {
        let ms: u64 = num.trim().parse().ok()?;
        return Some(Duration::from_millis(ms));
    }
    if let Some(num) = s.strip_suffix('s') {
        let secs: f64 = num.trim().parse().ok()?;
        return if secs >= 0.0 {
            Some(Duration::from_secs_f64(secs))
        } else {
            None
        };
    }
    if let Some(num) = s.strip_suffix('m') {
        let mins: f64 = num.trim().parse().ok()?;
        return if mins >= 0.0 {
            Some(Duration::from_secs_f64(mins * 60.0))
        } else {
            None
        };
    }
    if let Some(num) = s.strip_suffix('h') {
        let hours: f64 = num.trim().parse().ok()?;
        return if hours >= 0.0 {
            Some(Duration::from_secs_f64(hours * 3600.0))
        } else {
            None
        };
    }

    None
}

/// Convert a Value to a string.
fn value_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::String(s) => s.clone(),
        Value::Json(json) => json.to_string(),
        Value::Blob(blob) => format!("[blob: {} {}]", blob.formatted_size(), blob.content_type),
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

    // --- Duration parsing tests ---

    #[test]
    fn test_parse_duration_seconds() {
        assert_eq!(parse_duration("30"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("0"), Some(Duration::from_secs(0)));
        assert_eq!(parse_duration("1.5"), Some(Duration::from_secs_f64(1.5)));
    }

    #[test]
    fn test_parse_duration_suffix() {
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("5s"), Some(Duration::from_secs(5)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
    }

    #[test]
    fn test_parse_duration_invalid() {
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("-5"), None);
        assert_eq!(parse_duration("5x"), None);
    }

    // --- Tool execution tests ---

    #[tokio::test]
    async fn test_timeout_missing_args() {
        let mut ctx = make_ctx();
        let args = ToolArgs::new();

        let result = Timeout.execute(args, &mut ctx).await;
        assert!(!result.ok());
        assert!(result.err.contains("usage"));
    }

    #[tokio::test]
    async fn test_timeout_invalid_duration() {
        let mut ctx = make_ctx();
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("abc".into()));
        args.positional.push(Value::String("echo".into()));

        let result = Timeout.execute(args, &mut ctx).await;
        assert!(!result.ok());
        assert!(result.err.contains("invalid duration"));
    }

    #[tokio::test]
    async fn test_timeout_builtin_succeeds() {
        let mut ctx = make_ctx();
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("5s".into()));
        args.positional.push(Value::String("echo".into()));
        args.positional.push(Value::String("hello".into()));

        let result = Timeout.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert!(result.text_out().contains("hello"));
    }

    #[tokio::test]
    async fn test_timeout_builtin_times_out() {
        let mut ctx = make_ctx();
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("100ms".into()));
        args.positional.push(Value::String("sleep".into()));
        args.positional.push(Value::String("10".into()));

        let result = Timeout.execute(args, &mut ctx).await;
        assert_eq!(result.code, 124);
        assert!(result.err.contains("timed out"));
    }

    #[tokio::test]
    async fn test_timeout_command_not_found() {
        let mut ctx = make_ctx();
        let mut args = ToolArgs::new();
        args.positional.push(Value::String("5s".into()));
        args.positional
            .push(Value::String("not_a_command_xyz_123".into()));

        let result = Timeout.execute(args, &mut ctx).await;
        assert!(!result.ok());
        assert_eq!(result.code, 127);
    }

    #[tokio::test]
    async fn test_timeout_numeric_duration() {
        let mut ctx = make_ctx();
        let mut args = ToolArgs::new();
        // Numeric literal (lexer produces Int)
        args.positional.push(Value::Int(5));
        args.positional.push(Value::String("echo".into()));
        args.positional.push(Value::String("works".into()));

        let result = Timeout.execute(args, &mut ctx).await;
        assert!(result.ok());
        assert!(result.text_out().contains("works"));
    }
}
