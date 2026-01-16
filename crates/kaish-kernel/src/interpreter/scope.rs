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
}

impl Scope {
    /// Create a new scope with one empty frame.
    pub fn new() -> Self {
        Self {
            frames: vec![HashMap::new()],
            last_result: ExecResult::default(),
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

    /// Set the last command result (accessible via `$?`).
    pub fn set_last_result(&mut self, result: ExecResult) {
        self.last_result = result;
    }

    /// Get the last command result.
    pub fn last_result(&self) -> &ExecResult {
        &self.last_result
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
    fn resolve_result_path(&self, segments: &[VarSegment]) -> Option<Value> {
        if segments.is_empty() {
            // Return the full result as an object
            return Some(self.result_to_value());
        }

        // First segment must be a field access
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

    /// Convert the last result to a Value::Object for `$?` access.
    fn result_to_value(&self) -> Value {
        let result = &self.last_result;
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
}
