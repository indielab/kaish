//! Variable scope management for kaish.
//!
//! Scopes provide variable bindings with:
//! - Nested scope frames (push/pop for loops, tool calls)
//! - The special `$?` variable tracking the last command result
//! - Path resolution for nested access (`${VAR.field[0]}`)

use std::collections::HashMap;

use crate::ast::{Expr, Value, VarPath, VarSegment};

use super::result::ExecResult;

/// Variable scope with nested frames and last-result tracking.
///
/// Variables are looked up from innermost to outermost frame.
/// The `?` variable always refers to the last command result.
#[derive(Debug, Clone)]
pub struct Scope {
    /// Stack of variable frames. Last element is the innermost scope.
    frames: Vec<HashMap<String, Value>>,
    /// The result of the last command execution.
    last_result: ExecResult,
    /// Script or tool name ($0).
    script_name: String,
    /// Positional arguments ($1-$9, $@, $#).
    positional: Vec<String>,
    /// Error exit mode (set -e): exit on any command failure.
    error_exit: bool,
}

impl Scope {
    /// Create a new scope with one empty frame.
    pub fn new() -> Self {
        Self {
            frames: vec![HashMap::new()],
            last_result: ExecResult::default(),
            script_name: String::new(),
            positional: Vec::new(),
            error_exit: false,
        }
    }

    /// Push a new scope frame (for entering a loop, tool call, etc.)
    pub fn push_frame(&mut self) {
        self.frames.push(HashMap::new());
    }

    /// Pop the innermost scope frame.
    ///
    /// Panics if attempting to pop the last frame.
    pub fn pop_frame(&mut self) {
        if self.frames.len() > 1 {
            self.frames.pop();
        } else {
            panic!("cannot pop the root scope frame");
        }
    }

    /// Set a variable in the current (innermost) frame.
    pub fn set(&mut self, name: impl Into<String>, value: Value) {
        if let Some(frame) = self.frames.last_mut() {
            frame.insert(name.into(), value);
        }
    }

    /// Get a variable by name, searching from innermost to outermost frame.
    pub fn get(&self, name: &str) -> Option<&Value> {
        for frame in self.frames.iter().rev() {
            if let Some(value) = frame.get(name) {
                return Some(value);
            }
        }
        None
    }

    /// Remove a variable, searching from innermost to outermost frame.
    ///
    /// Returns the removed value if found, None otherwise.
    pub fn remove(&mut self, name: &str) -> Option<Value> {
        for frame in self.frames.iter_mut().rev() {
            if let Some(value) = frame.remove(name) {
                return Some(value);
            }
        }
        None
    }

    /// Set the last command result (accessible via `$?`).
    pub fn set_last_result(&mut self, result: ExecResult) {
        self.last_result = result;
    }

    /// Get the last command result.
    pub fn last_result(&self) -> &ExecResult {
        &self.last_result
    }

    /// Set the positional parameters ($0, $1-$9, $@, $#).
    ///
    /// The script_name becomes $0, and args become $1, $2, etc.
    pub fn set_positional(&mut self, script_name: impl Into<String>, args: Vec<String>) {
        self.script_name = script_name.into();
        self.positional = args;
    }

    /// Get a positional parameter by index ($0-$9).
    ///
    /// $0 returns the script name, $1-$9 return arguments.
    pub fn get_positional(&self, n: usize) -> Option<&str> {
        if n == 0 {
            if self.script_name.is_empty() {
                None
            } else {
                Some(&self.script_name)
            }
        } else {
            self.positional.get(n - 1).map(|s| s.as_str())
        }
    }

    /// Get all positional arguments as a slice ($@).
    pub fn all_args(&self) -> &[String] {
        &self.positional
    }

    /// Get the count of positional arguments ($#).
    pub fn arg_count(&self) -> usize {
        self.positional.len()
    }

    /// Check if error-exit mode is enabled (set -e).
    pub fn error_exit_enabled(&self) -> bool {
        self.error_exit
    }

    /// Set error-exit mode (set -e / set +e).
    pub fn set_error_exit(&mut self, enabled: bool) {
        self.error_exit = enabled;
    }

    /// Resolve a variable path like `${VAR.field[0].nested}`.
    ///
    /// Returns None if the path cannot be resolved.
    pub fn resolve_path(&self, path: &VarPath) -> Option<Value> {
        if path.segments.is_empty() {
            return None;
        }

        // Get the root variable name
        let root_name = match &path.segments[0] {
            VarSegment::Field(name) => name,
            VarSegment::Index(_) => return None, // Path must start with a name
        };

        // Special case: $? (last result)
        let root_value = if root_name == "?" {
            // $? returns the full result as an object, but we handle
            // field access specially in the remaining path resolution
            return self.resolve_result_path(&path.segments[1..]);
        } else {
            self.get(root_name)?.clone()
        };

        // Resolve remaining path segments
        self.resolve_value_path(root_value, &path.segments[1..])
    }

    /// Resolve path segments on the last result ($?).
    ///
    /// `$?` alone returns the exit code as an integer (0-255).
    /// For structured result access, use command substitution: `RESULT=$(cmd); ${RESULT.field}`
    fn resolve_result_path(&self, segments: &[VarSegment]) -> Option<Value> {
        if segments.is_empty() {
            // $? alone returns just the exit code as an integer (bash-compatible)
            return Some(Value::Int(self.last_result.code));
        }

        // Allow ${?.code}, ${?.ok}, etc. for backward compatibility (but $? alone is int)
        let field_name = match &segments[0] {
            VarSegment::Field(name) => name,
            VarSegment::Index(_) => return None,
        };

        // Get the field value from the result
        let field_value = self.last_result.get_field(field_name)?;

        // Continue resolving remaining segments
        self.resolve_value_path(field_value, &segments[1..])
    }

    /// Resolve path segments on a value.
    fn resolve_value_path(&self, value: Value, segments: &[VarSegment]) -> Option<Value> {
        if segments.is_empty() {
            return Some(value);
        }

        let next_value = match (&value, &segments[0]) {
            // Object field access: ${obj.field}
            (Value::Object(fields), VarSegment::Field(name)) => {
                fields
                    .iter()
                    .find(|(k, _)| k == name)
                    .and_then(|(_, expr)| self.expr_to_value(expr))
            }
            // Array index: ${arr[0]}
            (Value::Array(items), VarSegment::Index(idx)) => {
                items.get(*idx).and_then(|expr| self.expr_to_value(expr))
            }
            // Cannot index into other types
            _ => None,
        }?;

        self.resolve_value_path(next_value, &segments[1..])
    }

    /// Convert an Expr to a Value (only for literals).
    fn expr_to_value(&self, expr: &Expr) -> Option<Value> {
        match expr {
            Expr::Literal(v) => Some(v.clone()),
            _ => None, // Other expr types need evaluation
        }
    }

    /// Check if a variable exists in any frame.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Get all variable names in scope (for debugging/introspection).
    pub fn all_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .frames
            .iter()
            .flat_map(|f| f.keys().map(|s| s.as_str()))
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Get all variables as (name, value) pairs.
    ///
    /// Variables are deduplicated, with inner frames shadowing outer ones.
    pub fn all(&self) -> Vec<(String, Value)> {
        let mut result = std::collections::HashMap::new();
        // Iterate outer to inner so inner frames override
        for frame in &self.frames {
            for (name, value) in frame {
                result.insert(name.clone(), value.clone());
            }
        }
        let mut pairs: Vec<_> = result.into_iter().collect();
        pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
        pairs
    }
}

impl Default for Scope {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_scope_has_one_frame() {
        let scope = Scope::new();
        assert_eq!(scope.frames.len(), 1);
    }

    #[test]
    fn set_and_get_variable() {
        let mut scope = Scope::new();
        scope.set("X", Value::Int(42));
        assert_eq!(scope.get("X"), Some(&Value::Int(42)));
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let scope = Scope::new();
        assert_eq!(scope.get("MISSING"), None);
    }

    #[test]
    fn inner_frame_shadows_outer() {
        let mut scope = Scope::new();
        scope.set("X", Value::Int(1));
        scope.push_frame();
        scope.set("X", Value::Int(2));
        assert_eq!(scope.get("X"), Some(&Value::Int(2)));
        scope.pop_frame();
        assert_eq!(scope.get("X"), Some(&Value::Int(1)));
    }

    #[test]
    fn inner_frame_can_see_outer_vars() {
        let mut scope = Scope::new();
        scope.set("OUTER", Value::String("visible".into()));
        scope.push_frame();
        assert_eq!(scope.get("OUTER"), Some(&Value::String("visible".into())));
    }

    #[test]
    fn resolve_simple_path() {
        let mut scope = Scope::new();
        scope.set("NAME", Value::String("Alice".into()));

        let path = VarPath::simple("NAME");
        assert_eq!(
            scope.resolve_path(&path),
            Some(Value::String("Alice".into()))
        );
    }

    #[test]
    fn resolve_object_field() {
        let mut scope = Scope::new();
        scope.set(
            "USER",
            Value::Object(vec![
                ("name".into(), Expr::Literal(Value::String("Bob".into()))),
                ("age".into(), Expr::Literal(Value::Int(30))),
            ]),
        );

        let path = VarPath {
            segments: vec![
                VarSegment::Field("USER".into()),
                VarSegment::Field("name".into()),
            ],
        };
        assert_eq!(
            scope.resolve_path(&path),
            Some(Value::String("Bob".into()))
        );
    }

    #[test]
    fn resolve_array_index() {
        let mut scope = Scope::new();
        scope.set(
            "ITEMS",
            Value::Array(vec![
                Expr::Literal(Value::String("first".into())),
                Expr::Literal(Value::String("second".into())),
            ]),
        );

        let path = VarPath {
            segments: vec![
                VarSegment::Field("ITEMS".into()),
                VarSegment::Index(1),
            ],
        };
        assert_eq!(
            scope.resolve_path(&path),
            Some(Value::String("second".into()))
        );
    }

    #[test]
    fn resolve_nested_path() {
        let mut scope = Scope::new();
        scope.set(
            "DATA",
            Value::Object(vec![(
                "users".into(),
                Expr::Literal(Value::Array(vec![
                    Expr::Literal(Value::Object(vec![
                        ("name".into(), Expr::Literal(Value::String("Alice".into()))),
                    ])),
                ])),
            )]),
        );

        // ${DATA.users[0].name}
        let path = VarPath {
            segments: vec![
                VarSegment::Field("DATA".into()),
                VarSegment::Field("users".into()),
                VarSegment::Index(0),
                VarSegment::Field("name".into()),
            ],
        };
        assert_eq!(
            scope.resolve_path(&path),
            Some(Value::String("Alice".into()))
        );
    }

    #[test]
    fn resolve_last_result_ok() {
        let mut scope = Scope::new();
        scope.set_last_result(ExecResult::success("output"));

        let path = VarPath {
            segments: vec![
                VarSegment::Field("?".into()),
                VarSegment::Field("ok".into()),
            ],
        };
        assert_eq!(scope.resolve_path(&path), Some(Value::Bool(true)));
    }

    #[test]
    fn resolve_last_result_code() {
        let mut scope = Scope::new();
        scope.set_last_result(ExecResult::failure(127, "not found"));

        let path = VarPath {
            segments: vec![
                VarSegment::Field("?".into()),
                VarSegment::Field("code".into()),
            ],
        };
        assert_eq!(scope.resolve_path(&path), Some(Value::Int(127)));
    }

    #[test]
    fn resolve_last_result_data_field() {
        let mut scope = Scope::new();
        scope.set_last_result(ExecResult::success(r#"{"count": 5}"#));

        // ${?.data.count}
        let path = VarPath {
            segments: vec![
                VarSegment::Field("?".into()),
                VarSegment::Field("data".into()),
                VarSegment::Field("count".into()),
            ],
        };
        assert_eq!(scope.resolve_path(&path), Some(Value::Int(5)));
    }

    #[test]
    fn resolve_invalid_path_returns_none() {
        let mut scope = Scope::new();
        scope.set("X", Value::Int(42));

        // Cannot do field access on an int
        let path = VarPath {
            segments: vec![
                VarSegment::Field("X".into()),
                VarSegment::Field("invalid".into()),
            ],
        };
        assert_eq!(scope.resolve_path(&path), None);
    }

    #[test]
    fn resolve_out_of_bounds_index_returns_none() {
        let mut scope = Scope::new();
        scope.set(
            "ARR",
            Value::Array(vec![Expr::Literal(Value::Int(1))]),
        );

        let path = VarPath {
            segments: vec![
                VarSegment::Field("ARR".into()),
                VarSegment::Index(99),
            ],
        };
        assert_eq!(scope.resolve_path(&path), None);
    }

    #[test]
    fn contains_finds_variable() {
        let mut scope = Scope::new();
        scope.set("EXISTS", Value::Bool(true));
        assert!(scope.contains("EXISTS"));
        assert!(!scope.contains("MISSING"));
    }

    #[test]
    fn all_names_lists_variables() {
        let mut scope = Scope::new();
        scope.set("A", Value::Int(1));
        scope.set("B", Value::Int(2));
        scope.push_frame();
        scope.set("C", Value::Int(3));

        let names = scope.all_names();
        assert!(names.contains(&"A"));
        assert!(names.contains(&"B"));
        assert!(names.contains(&"C"));
    }

    #[test]
    #[should_panic(expected = "cannot pop the root scope frame")]
    fn pop_root_frame_panics() {
        let mut scope = Scope::new();
        scope.pop_frame();
    }

    #[test]
    fn positional_params_basic() {
        let mut scope = Scope::new();
        scope.set_positional("my_tool", vec!["arg1".into(), "arg2".into(), "arg3".into()]);

        // $0 is the script/tool name
        assert_eq!(scope.get_positional(0), Some("my_tool"));
        // $1, $2, $3 are the arguments
        assert_eq!(scope.get_positional(1), Some("arg1"));
        assert_eq!(scope.get_positional(2), Some("arg2"));
        assert_eq!(scope.get_positional(3), Some("arg3"));
        // $4 doesn't exist
        assert_eq!(scope.get_positional(4), None);
    }

    #[test]
    fn positional_params_empty() {
        let scope = Scope::new();
        // No positional params set
        assert_eq!(scope.get_positional(0), None);
        assert_eq!(scope.get_positional(1), None);
        assert_eq!(scope.arg_count(), 0);
        assert!(scope.all_args().is_empty());
    }

    #[test]
    fn all_args_returns_slice() {
        let mut scope = Scope::new();
        scope.set_positional("test", vec!["a".into(), "b".into(), "c".into()]);

        let args = scope.all_args();
        assert_eq!(args, &["a", "b", "c"]);
    }

    #[test]
    fn arg_count_returns_count() {
        let mut scope = Scope::new();
        scope.set_positional("test", vec!["one".into(), "two".into()]);

        assert_eq!(scope.arg_count(), 2);
    }
}
