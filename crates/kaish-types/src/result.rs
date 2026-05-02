//! ExecResult — the structured result of every command execution.
//!
//! After every command in kaish, the special variable `$?` contains an ExecResult.

use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::output::OutputData;
use crate::value::Value;

/// The result of executing a command or pipeline.
///
/// Fields accessible via `${?.field}`:
/// - `code` — exit code (0 = success)
/// - `ok` — true if code == 0
/// - `err` — error message if failed
/// - `out` — raw stdout as string
/// - `data` — structured data; only set by builtins/tools that opt in
///   (e.g. `seq`, `jq`, `cut`, `find`, `glob`, `split`). External commands
///   never populate this — pipe their stdout through `jq` to get it.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExecResult {
    /// Exit code. 0 means success.
    pub code: i64,
    /// Raw standard output as a string (canonical for pipes).
    out: String,
    /// Raw standard error as a string.
    pub err: String,
    /// Structured data — only populated when a builtin/tool sets it explicitly.
    /// Stdout is *never* sniffed; this stays `None` for external commands.
    pub data: Option<Value>,
    /// Structured output data for rendering.
    output: Option<OutputData>,
    /// True if output was truncated and written to a spill file.
    pub did_spill: bool,
    /// The command's original exit code before spill logic overwrote it with 2 or 3.
    /// Present only when `did_spill` is true and `code` was changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_code: Option<i64>,
    /// MIME content type hint (e.g., "text/markdown", "image/svg+xml").
    /// When set, downstream consumers can use this instead of sniffing content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Opaque key-value context propagated from tools through execution.
    /// Intermediaries (kaish) carry but don't interpret. Consumers read known keys.
    /// Follows W3C Baggage semantics — useful for OTel trace propagation,
    /// application-level hints, etc.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub baggage: BTreeMap<String, String>,
}

impl ExecResult {
    /// Create a successful result with output.
    pub fn success(out: impl Into<String>) -> Self {
        Self {
            code: 0,
            out: out.into(),
            err: String::new(),
            data: None,
            output: None,
            did_spill: false,
            original_code: None,
            content_type: None,
            baggage: BTreeMap::new(),
        }
    }

    /// Create a successful result with structured output data.
    ///
    /// The `OutputData` is the source of truth. Text is materialized lazily
    /// via `text_out()` when needed (pipes, redirects, command substitution).
    pub fn with_output(output: OutputData) -> Self {
        // Simple text: move string into .out directly for efficient Cow::Borrowed.
        // Structured output: store in .output, materialize lazily.
        match output.into_text() {
            Ok(text) => Self::success(text),
            Err(output) => Self {
                code: 0,
                out: String::new(),
                err: String::new(),
                data: None,
                output: Some(output),
                did_spill: false,
                original_code: None,
                content_type: None,
                baggage: BTreeMap::new(),
            },
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
            output: None,
            did_spill: false,
            original_code: None,
            content_type: None,
            baggage: BTreeMap::new(),
        }
    }

    /// Create a successful result with both text output and structured data.
    ///
    /// Use this when a command should have:
    /// - Text output for pipes and traditional shell usage
    /// - Structured data for iteration and programmatic access
    ///
    /// The data field takes precedence for command substitution in contexts
    /// like `for i in $(cmd)` where the structured data can be iterated.
    pub fn success_with_data(out: impl Into<String>, data: Value) -> Self {
        Self {
            code: 0,
            out: out.into(),
            err: String::new(),
            data: Some(data),
            output: None,
            did_spill: false,
            original_code: None,
            content_type: None,
            baggage: BTreeMap::new(),
        }
    }

    /// Create a failed result with an error message.
    pub fn failure(code: i64, err: impl Into<String>) -> Self {
        Self {
            code,
            out: String::new(),
            err: err.into(),
            data: None,
            output: None,
            did_spill: false,
            original_code: None,
            content_type: None,
            baggage: BTreeMap::new(),
        }
    }

    /// Create a result from raw output streams.
    ///
    /// `data` is left empty — kaish does not sniff stdout for JSON. To get
    /// structured iteration from an external command, pipe through `jq`:
    /// `for i in $(curl ... | jq .); do ...`.
    pub fn from_output(code: i64, stdout: impl Into<String>, stderr: impl Into<String>) -> Self {
        Self {
            code,
            out: stdout.into(),
            err: stderr.into(),
            data: None,
            output: None,
            did_spill: false,
            original_code: None,
            content_type: None,
            baggage: BTreeMap::new(),
        }
    }

    /// Create a successful result with structured output and explicit pipe text.
    ///
    /// Use this when a builtin needs custom text formatting that differs from
    /// the canonical `OutputData::to_canonical_string()` representation.
    pub fn with_output_and_text(output: OutputData, text: impl Into<String>) -> Self {
        Self {
            code: 0,
            out: text.into(),
            err: String::new(),
            data: None,
            output: Some(output),
            did_spill: false,
            original_code: None,
            content_type: None,
            baggage: BTreeMap::new(),
        }
    }

    /// Create a result from parts — for kernel struct literal sites.
    pub fn from_parts(
        code: i64,
        out: String,
        err: String,
        data: Option<Value>,
    ) -> Self {
        Self {
            code,
            out,
            err,
            data,
            output: None,
            did_spill: false,
            original_code: None,
            content_type: None,
            baggage: BTreeMap::new(),
        }
    }

    /// Builder: set the exit code, returning self for chaining.
    pub fn with_code(mut self, code: i64) -> Self {
        self.code = code;
        self
    }

    // ── Read accessors ──

    /// Get text output, materializing from OutputData on demand.
    ///
    /// Returns `self.out` if non-empty, otherwise falls back to
    /// `OutputData::to_canonical_string()`. This is the canonical way to
    /// get text for pipes, command substitution, and file redirects.
    pub fn text_out(&self) -> Cow<'_, str> {
        if !self.out.is_empty() {
            Cow::Borrowed(&self.out)
        } else if let Some(ref output) = self.output {
            Cow::Owned(output.to_canonical_string())
        } else {
            Cow::Borrowed("")
        }
    }

    /// Get a reference to structured output data.
    pub fn output(&self) -> Option<&OutputData> {
        self.output.as_ref()
    }

    /// True if structured output data is present.
    pub fn has_output(&self) -> bool {
        self.output.is_some()
    }

    // ── Mutation accessors ──

    /// Replace `.out` with a new string.
    pub fn set_out(&mut self, s: String) {
        self.out = s;
    }

    /// Append to `.out`.
    pub fn push_out(&mut self, s: &str) {
        self.out.push_str(s);
    }

    /// Clear `.out`.
    pub fn clear_out(&mut self) {
        self.out.clear();
    }

    /// Replace `.output`.
    pub fn set_output(&mut self, o: Option<OutputData>) {
        self.output = o;
    }

    /// Take `.output`, leaving None.
    pub fn take_output(&mut self) -> Option<OutputData> {
        self.output.take()
    }

    /// Materialize: if `.out` is empty and `.output` is present,
    /// populate `.out` from canonical string and clear `.output`.
    pub fn materialize(&mut self) {
        if self.out.is_empty() {
            if let Some(ref output) = self.output {
                self.out = output.to_canonical_string();
            }
        }
        self.output = None;
    }

    /// Take `.output` only if `.out` is empty (no custom text),
    /// so caller can stream directly without materializing.
    pub fn take_output_for_stream(&mut self) -> Option<OutputData> {
        if self.out.is_empty() {
            self.output.take()
        } else {
            None
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
            "out" => Some(Value::String(self.text_out().into_owned())),
            "err" => Some(Value::String(self.err.clone())),
            "data" => self.data.clone(),
            _ => None,
        }
    }

    /// Set content type hint, returning self for chaining.
    pub fn with_content_type(mut self, ct: impl Into<String>) -> Self {
        self.content_type = Some(ct.into());
        self
    }

}

/// Convert serde_json::Value to our AST Value.
///
/// Primitives are mapped to their corresponding Value variants.
/// Arrays and objects are preserved as `Value::Json` - use `jq` to query them.
pub fn json_to_value(json: serde_json::Value) -> Value {
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
        // Arrays and objects are preserved as Json values
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => Value::Json(json),
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
        Value::Json(json) => json.clone(),
        Value::Blob(blob) => {
            let mut map = serde_json::Map::new();
            map.insert("_type".to_string(), serde_json::Value::String("blob".to_string()));
            map.insert("id".to_string(), serde_json::Value::String(blob.id.clone()));
            map.insert("size".to_string(), serde_json::Value::Number(blob.size.into()));
            map.insert("contentType".to_string(), serde_json::Value::String(blob.content_type.clone()));
            if let Some(hash) = &blob.hash {
                let hash_hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
                map.insert("hash".to_string(), serde_json::Value::String(hash_hex));
            }
            serde_json::Value::Object(map)
        }
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
    fn success_does_not_sniff_json_stdout() {
        // External-command stdout is never sniffed for JSON. Tools that want
        // structured data must call success_with_data() / success_data().
        let result = ExecResult::success(r#"{"count": 42, "items": ["a", "b"]}"#);
        assert!(result.data.is_none());
        assert_eq!(result.out, r#"{"count": 42, "items": ["a", "b"]}"#);
    }

    #[test]
    fn from_output_does_not_sniff_json_stdout() {
        let result = ExecResult::from_output(0, r#"[1, 2, 3]"#, "");
        assert!(result.data.is_none());
        assert_eq!(result.out, "[1, 2, 3]");
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
        // .data is only populated by tools that opt in — wire it explicitly here.
        let value = Value::Json(serde_json::json!({"key": "value"}));
        let result = ExecResult::success_data(value);
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

    #[test]
    fn did_spill_defaults_to_false() {
        assert!(!ExecResult::success("hi").did_spill);
        assert!(!ExecResult::failure(1, "err").did_spill);
        assert!(!ExecResult::from_output(0, "out", "err").did_spill);
    }

    #[test]
    fn did_spill_is_serialized() {
        let mut result = ExecResult::success("hi");
        result.did_spill = true;
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"did_spill\":true"));
    }

    #[test]
    fn original_code_omitted_when_none() {
        let result = ExecResult::success("hi");
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("original_code"));
    }

    #[test]
    fn original_code_present_when_set() {
        let mut result = ExecResult::success("hi");
        result.original_code = Some(0);
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"original_code\":0"));
    }

    #[test]
    fn default_is_empty_success() {
        let result = ExecResult::default();
        assert!(result.ok());
        assert!(result.out.is_empty());
        assert!(result.data.is_none());
        assert!(result.content_type.is_none());
        assert!(result.baggage.is_empty());
    }

    #[test]
    fn from_parts_creates_result() {
        let result = ExecResult::from_parts(42, "out".into(), "err".into(), None);
        assert_eq!(result.code, 42);
        assert_eq!(result.out, "out");
        assert_eq!(result.err, "err");
        assert!(result.data.is_none());
        assert!(result.output.is_none());
    }

    #[test]
    fn with_code_sets_code() {
        let result = ExecResult::success("hi").with_code(42);
        assert_eq!(result.code, 42);
        assert_eq!(result.out, "hi");
    }

    #[test]
    fn output_getter() {
        use crate::output::{OutputData, OutputNode};
        // Use structured (non-text) output so with_output preserves .output
        let nodes = OutputData::nodes(vec![OutputNode::new("a"), OutputNode::new("b")]);
        let result = ExecResult::with_output(nodes);
        assert!(result.output().is_some());
        assert!(result.has_output());

        // Simple text now routes to .out, so output is None
        let text_result = ExecResult::with_output(OutputData::text("test"));
        assert!(!text_result.has_output());
        assert_eq!(&*text_result.text_out(), "test");

        let plain = ExecResult::success("text");
        assert!(plain.output().is_none());
        assert!(!plain.has_output());
    }

    #[test]
    fn set_out_and_push_out_and_clear_out() {
        let mut result = ExecResult::success("");
        result.set_out("hello".into());
        assert_eq!(result.out, "hello");
        result.push_out(" world");
        assert_eq!(result.out, "hello world");
        result.clear_out();
        assert!(result.out.is_empty());
    }

    #[test]
    fn set_output_and_take_output() {
        use crate::output::OutputData;
        let mut result = ExecResult::success("");
        assert!(result.take_output().is_none());

        result.set_output(Some(OutputData::text("data")));
        assert!(result.has_output());

        let taken = result.take_output();
        assert!(taken.is_some());
        assert!(!result.has_output());
    }

    #[test]
    fn materialize_populates_out_from_output() {
        use crate::output::{OutputData, OutputNode};
        // Use structured output to test materialization
        let nodes = OutputData::nodes(vec![OutputNode::new("a"), OutputNode::new("b")]);
        let mut result = ExecResult::with_output(nodes);
        assert!(result.out.is_empty());
        assert!(result.has_output());
        result.materialize();
        assert_eq!(result.out, "a\nb");
        assert!(result.output.is_none());
    }

    #[test]
    fn materialize_preserves_existing_out() {
        use crate::output::OutputData;
        let mut result = ExecResult::with_output_and_text(OutputData::text("ignored"), "custom");
        result.materialize();
        assert_eq!(result.out, "custom");
    }

    #[test]
    fn take_output_for_stream_when_out_empty() {
        use crate::output::{OutputData, OutputNode};
        // Use structured output — text now goes to .out directly
        let nodes = OutputData::nodes(vec![OutputNode::new("a")]);
        let mut result = ExecResult::with_output(nodes);
        let taken = result.take_output_for_stream();
        assert!(taken.is_some());
        assert!(!result.has_output());
    }

    #[test]
    fn with_output_simple_text_populates_out_directly() {
        use crate::output::OutputData;
        let result = ExecResult::with_output(OutputData::text("hello"));
        // Simple text should go to .out, not .output
        assert!(!result.has_output());
        assert_eq!(&*result.text_out(), "hello");
        // Even JSON-shaped text is NOT auto-parsed — .data stays None.
        let json_result = ExecResult::with_output(OutputData::text(r#"{"key": 1}"#));
        assert!(json_result.data.is_none());
    }

    #[test]
    fn take_output_for_stream_when_out_populated() {
        use crate::output::OutputData;
        let mut result = ExecResult::with_output_and_text(OutputData::text("x"), "custom");
        let taken = result.take_output_for_stream();
        assert!(taken.is_none());
        assert!(result.has_output()); // not taken
    }
}
