//! ExecResult — the structured result of every command execution.
//!
//! After every command in kaish, the special variable `$?` contains an ExecResult:
//!
//! ```kaish
//! api-call endpoint=/users
//! if ${?.ok}; then
//!     echo "Got ${?.data.count} users"
//! else
//!     echo "Error: ${?.err}"
//! fi
//! ```
//!
//! This differs from traditional shells where `$?` is just an integer exit code.
//! In kaish, we capture the full context: exit code, stdout, parsed data, and errors.

use crate::ast::Value;

/// The result of executing a command or pipeline.
///
/// Fields accessible via `${?.field}`:
/// - `code` — exit code (0 = success)
/// - `ok` — true if code == 0
/// - `err` — error message if failed
/// - `out` — raw stdout as string
/// - `data` — parsed JSON from stdout (if valid JSON)
#[derive(Debug, Clone, PartialEq)]
pub struct ExecResult {
    /// Exit code. 0 means success.
    pub code: i64,
    /// Raw standard output as a string.
    pub out: String,
    /// Raw standard error as a string.
    pub err: String,
    /// Parsed JSON data from stdout, if stdout was valid JSON.
    pub data: Option<Value>,
}

impl ExecResult {
    /// Create a successful result with output.
    pub fn success(out: impl Into<String>) -> Self {
        let out = out.into();
        let data = Self::try_parse_json(&out);
        Self {
            code: 0,
            out,
            err: String::new(),
            data,
        }
    }

    /// Create a successful result with structured data.
    pub fn success_data(data: Value) -> Self {
        let out = value_to_json(&data).to_string();
        Self {
            code: 0,
            out,
            err: String::new(),
            data: Some(data),
        }
    }

    /// Create a failed result with an error message.
    pub fn failure(code: i64, err: impl Into<String>) -> Self {
        Self {
            code,
            out: String::new(),
            err: err.into(),
            data: None,
        }
    }

    /// Create a result from raw output streams.
    pub fn from_output(code: i64, stdout: impl Into<String>, stderr: impl Into<String>) -> Self {
        let out = stdout.into();
        let data = if code == 0 {
            Self::try_parse_json(&out)
        } else {
            None
        };
        Self {
            code,
            out,
            err: stderr.into(),
            data,
        }
    }

    /// True if the command succeeded (exit code 0).
    pub fn ok(&self) -> bool {
        self.code == 0
    }

    /// Get a field by name, for variable access like `${?.field}`.
    pub fn get_field(&self, name: &str) -> Option<Value> {
        match name {
            "code" => Some(Value::Int(self.code)),
            "ok" => Some(Value::Bool(self.ok())),
            "out" => Some(Value::String(self.out.clone())),
            "err" => Some(Value::String(self.err.clone())),
            "data" => self.data.clone(),
            _ => None,
        }
    }

    /// Try to parse a string as JSON, returning a Value if successful.
    fn try_parse_json(s: &str) -> Option<Value> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        serde_json::from_str::<serde_json::Value>(trimmed)
            .ok()
            .map(json_to_value)
    }
}

impl Default for ExecResult {
    fn default() -> Self {
        Self::success("")
    }
}

/// Convert serde_json::Value to our AST Value.
///
/// Arrays and objects are stringified - use `jq` to extract values.
fn json_to_value(json: serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::String(n.to_string())
            }
        }
        serde_json::Value::String(s) => Value::String(s),
        // Arrays and objects are stored as JSON strings
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            Value::String(json.to_string())
        }
    }
}

/// Convert our AST Value to serde_json::Value for serialization.
pub fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => {
            serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)
        }
        Value::String(s) => serde_json::Value::String(s.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_creates_ok_result() {
        let result = ExecResult::success("hello world");
        assert!(result.ok());
        assert_eq!(result.code, 0);
        assert_eq!(result.out, "hello world");
        assert!(result.err.is_empty());
    }

    #[test]
    fn failure_creates_non_ok_result() {
        let result = ExecResult::failure(1, "command not found");
        assert!(!result.ok());
        assert_eq!(result.code, 1);
        assert_eq!(result.err, "command not found");
    }

    #[test]
    fn json_stdout_is_parsed() {
        // JSON objects/arrays are stored as JSON strings
        let result = ExecResult::success(r#"{"count": 42, "items": ["a", "b"]}"#);
        assert!(result.data.is_some());
        let data = result.data.unwrap();
        // Objects are stored as stringified JSON
        assert!(matches!(data, Value::String(_)));
    }

    #[test]
    fn non_json_stdout_has_no_data() {
        let result = ExecResult::success("just plain text");
        assert!(result.data.is_none());
    }

    #[test]
    fn get_field_code() {
        let result = ExecResult::failure(127, "not found");
        assert_eq!(result.get_field("code"), Some(Value::Int(127)));
    }

    #[test]
    fn get_field_ok() {
        let success = ExecResult::success("hi");
        let failure = ExecResult::failure(1, "err");
        assert_eq!(success.get_field("ok"), Some(Value::Bool(true)));
        assert_eq!(failure.get_field("ok"), Some(Value::Bool(false)));
    }

    #[test]
    fn get_field_out_and_err() {
        let result = ExecResult::from_output(1, "stdout text", "stderr text");
        assert_eq!(result.get_field("out"), Some(Value::String("stdout text".into())));
        assert_eq!(result.get_field("err"), Some(Value::String("stderr text".into())));
    }

    #[test]
    fn get_field_data() {
        let result = ExecResult::success(r#"{"key": "value"}"#);
        let data = result.get_field("data");
        assert!(data.is_some());
    }

    #[test]
    fn get_field_unknown_returns_none() {
        let result = ExecResult::success("");
        assert_eq!(result.get_field("nonexistent"), None);
    }

    #[test]
    fn success_data_creates_result_with_value() {
        let value = Value::String("test data".into());
        let result = ExecResult::success_data(value.clone());
        assert!(result.ok());
        assert_eq!(result.data, Some(value));
    }
}
