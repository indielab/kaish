//! Expression evaluation for kaish.
//!
//! The evaluator takes AST expressions and reduces them to values.
//! Variable references are resolved through the Scope, and string
//! interpolation is expanded.
//!
//! Command substitution (`$(pipeline)`) requires an executor, which is
//! provided by higher layers (L6: Pipes & Jobs).

use std::fmt;

use crate::ast::{BinaryOp, Expr, Pipeline, StringPart, Value, VarPath};

use super::result::ExecResult;
use super::scope::Scope;

/// Errors that can occur during expression evaluation.
#[derive(Debug, Clone, PartialEq)]
pub enum EvalError {
    /// Variable not found in scope.
    UndefinedVariable(String),
    /// Path resolution failed (bad field/index access).
    InvalidPath(String),
    /// Type mismatch for operation.
    TypeError { expected: &'static str, got: String },
    /// Command substitution failed.
    CommandFailed(String),
    /// No executor available for command substitution.
    NoExecutor,
    /// Division by zero or similar arithmetic error.
    ArithmeticError(String),
    /// Invalid regex pattern.
    RegexError(String),
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalError::UndefinedVariable(name) => write!(f, "undefined variable: {name}"),
            EvalError::InvalidPath(path) => write!(f, "invalid path: {path}"),
            EvalError::TypeError { expected, got } => {
                write!(f, "type error: expected {expected}, got {got}")
            }
            EvalError::CommandFailed(msg) => write!(f, "command failed: {msg}"),
            EvalError::NoExecutor => write!(f, "no executor available for command substitution"),
            EvalError::ArithmeticError(msg) => write!(f, "arithmetic error: {msg}"),
            EvalError::RegexError(msg) => write!(f, "regex error: {msg}"),
        }
    }
}

impl std::error::Error for EvalError {}

/// Result type for evaluation.
pub type EvalResult<T> = Result<T, EvalError>;

/// Trait for executing pipelines (command substitution).
///
/// This is implemented by higher layers (L6: Pipes & Jobs) to provide
/// actual command execution. The evaluator calls this when it encounters
/// a `$(pipeline)` expression.
pub trait Executor {
    /// Execute a pipeline and return its result.
    ///
    /// The executor should:
    /// 1. Parse and execute the pipeline
    /// 2. Capture stdout/stderr
    /// 3. Return an ExecResult with code, output, and parsed data
    fn execute(&mut self, pipeline: &Pipeline, scope: &mut Scope) -> EvalResult<ExecResult>;
}

/// A stub executor that always returns an error.
///
/// Used in L3 before the full executor is available.
pub struct NoOpExecutor;

impl Executor for NoOpExecutor {
    fn execute(&mut self, _pipeline: &Pipeline, _scope: &mut Scope) -> EvalResult<ExecResult> {
        Err(EvalError::NoExecutor)
    }
}

/// Expression evaluator.
///
/// Evaluates AST expressions to values, using the provided scope for
/// variable lookup and the executor for command substitution.
pub struct Evaluator<'a, E: Executor> {
    scope: &'a mut Scope,
    executor: &'a mut E,
}

impl<'a, E: Executor> Evaluator<'a, E> {
    /// Create a new evaluator with the given scope and executor.
    pub fn new(scope: &'a mut Scope, executor: &'a mut E) -> Self {
        Self { scope, executor }
    }

    /// Evaluate an expression to a value.
    pub fn eval(&mut self, expr: &Expr) -> EvalResult<Value> {
        match expr {
            Expr::Literal(value) => self.eval_literal(value),
            Expr::VarRef(path) => self.eval_var_ref(path),
            Expr::Interpolated(parts) => self.eval_interpolated(parts),
            Expr::BinaryOp { left, op, right } => self.eval_binary_op(left, *op, right),
            Expr::CommandSubst(pipeline) => self.eval_command_subst(pipeline),
        }
    }

    /// Evaluate a literal value.
    ///
    /// Arrays and objects may contain expressions that need evaluation.
    fn eval_literal(&mut self, value: &Value) -> EvalResult<Value> {
        match value {
            Value::Array(items) => {
                let evaluated: Result<Vec<_>, _> = items
                    .iter()
                    .map(|expr| self.eval(expr).map(|v| Expr::Literal(v)))
                    .collect();
                Ok(Value::Array(evaluated?))
            }
            Value::Object(fields) => {
                let evaluated: Result<Vec<_>, _> = fields
                    .iter()
                    .map(|(k, expr)| self.eval(expr).map(|v| (k.clone(), Expr::Literal(v))))
                    .collect();
                Ok(Value::Object(evaluated?))
            }
            // Primitive values are returned as-is
            _ => Ok(value.clone()),
        }
    }

    /// Evaluate a variable reference.
    fn eval_var_ref(&mut self, path: &VarPath) -> EvalResult<Value> {
        self.scope
            .resolve_path(path)
            .ok_or_else(|| EvalError::InvalidPath(format_path(path)))
    }

    /// Evaluate an interpolated string.
    fn eval_interpolated(&mut self, parts: &[StringPart]) -> EvalResult<Value> {
        let mut result = String::new();
        for part in parts {
            match part {
                StringPart::Literal(s) => result.push_str(s),
                StringPart::Var(path) => {
                    let value = self.scope.resolve_path(path).ok_or_else(|| {
                        EvalError::InvalidPath(format_path(path))
                    })?;
                    result.push_str(&value_to_string(&value));
                }
            }
        }
        Ok(Value::String(result))
    }

    /// Evaluate a binary operation.
    fn eval_binary_op(&mut self, left: &Expr, op: BinaryOp, right: &Expr) -> EvalResult<Value> {
        match op {
            // Short-circuit logical operators
            BinaryOp::And => {
                let left_val = self.eval(left)?;
                if !is_truthy(&left_val) {
                    return Ok(left_val);
                }
                self.eval(right)
            }
            BinaryOp::Or => {
                let left_val = self.eval(left)?;
                if is_truthy(&left_val) {
                    return Ok(left_val);
                }
                self.eval(right)
            }
            // Comparison operators
            BinaryOp::Eq => {
                let left_val = self.eval(left)?;
                let right_val = self.eval(right)?;
                Ok(Value::Bool(values_equal(&left_val, &right_val)))
            }
            BinaryOp::NotEq => {
                let left_val = self.eval(left)?;
                let right_val = self.eval(right)?;
                Ok(Value::Bool(!values_equal(&left_val, &right_val)))
            }
            BinaryOp::Lt => {
                let left_val = self.eval(left)?;
                let right_val = self.eval(right)?;
                compare_values(&left_val, &right_val).map(|ord| Value::Bool(ord.is_lt()))
            }
            BinaryOp::Gt => {
                let left_val = self.eval(left)?;
                let right_val = self.eval(right)?;
                compare_values(&left_val, &right_val).map(|ord| Value::Bool(ord.is_gt()))
            }
            BinaryOp::LtEq => {
                let left_val = self.eval(left)?;
                let right_val = self.eval(right)?;
                compare_values(&left_val, &right_val).map(|ord| Value::Bool(ord.is_le()))
            }
            BinaryOp::GtEq => {
                let left_val = self.eval(left)?;
                let right_val = self.eval(right)?;
                compare_values(&left_val, &right_val).map(|ord| Value::Bool(ord.is_ge()))
            }
            // Regex match operators
            BinaryOp::Match => {
                let left_val = self.eval(left)?;
                let right_val = self.eval(right)?;
                regex_match(&left_val, &right_val, false)
            }
            BinaryOp::NotMatch => {
                let left_val = self.eval(left)?;
                let right_val = self.eval(right)?;
                regex_match(&left_val, &right_val, true)
            }
        }
    }

    /// Evaluate command substitution.
    fn eval_command_subst(&mut self, pipeline: &Pipeline) -> EvalResult<Value> {
        let result = self.executor.execute(pipeline, self.scope)?;

        // Update $? with the result
        self.scope.set_last_result(result.clone());

        // Return the result as a value (the result object itself)
        // The caller can access .ok, .data, etc.
        Ok(result_to_value(&result))
    }
}

/// Convert a Value to its string representation for interpolation.
fn value_to_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => {
            // For structured values, convert to JSON
            super::result::value_to_json(value).to_string()
        }
    }
}

/// Format a VarPath for error messages.
fn format_path(path: &VarPath) -> String {
    use crate::ast::VarSegment;
    let mut result = String::from("${");
    for (i, seg) in path.segments.iter().enumerate() {
        match seg {
            VarSegment::Field(name) => {
                if i > 0 {
                    result.push('.');
                }
                result.push_str(name);
            }
            VarSegment::Index(idx) => {
                result.push('[');
                result.push_str(&idx.to_string());
                result.push(']');
            }
        }
    }
    result.push('}');
    result
}

/// Check if a value is "truthy" for boolean operations.
///
/// - `null` → false
/// - `false` → false
/// - `0` → false
/// - `""` → false
/// - `[]` → false
/// - Everything else → true
fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Int(i) => *i != 0,
        Value::Float(f) => *f != 0.0,
        Value::String(s) => !s.is_empty(),
        Value::Array(arr) => !arr.is_empty(),
        Value::Object(_) => true, // Objects are always truthy
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
        // Arrays and objects use structural equality
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len()
                && a.iter().zip(b.iter()).all(|(ae, be)| {
                    match (ae, be) {
                        (Expr::Literal(av), Expr::Literal(bv)) => values_equal(av, bv),
                        _ => false,
                    }
                })
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter().all(|(k, ae)| {
                    b.iter().any(|(bk, be)| {
                        k == bk
                            && match (ae, be) {
                                (Expr::Literal(av), Expr::Literal(bv)) => values_equal(av, bv),
                                _ => false,
                            }
                    })
                })
        }
        _ => false,
    }
}

/// Compare two values for ordering.
fn compare_values(left: &Value, right: &Value) -> EvalResult<std::cmp::Ordering> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b)),
        (Value::Float(a), Value::Float(b)) => {
            a.partial_cmp(b).ok_or_else(|| EvalError::ArithmeticError("NaN comparison".into()))
        }
        (Value::Int(a), Value::Float(b)) => {
            (*a as f64).partial_cmp(b).ok_or_else(|| EvalError::ArithmeticError("NaN comparison".into()))
        }
        (Value::Float(a), Value::Int(b)) => {
            a.partial_cmp(&(*b as f64)).ok_or_else(|| EvalError::ArithmeticError("NaN comparison".into()))
        }
        (Value::String(a), Value::String(b)) => Ok(a.cmp(b)),
        _ => Err(EvalError::TypeError {
            expected: "comparable types (numbers or strings)",
            got: format!("{:?} vs {:?}", type_name(left), type_name(right)),
        }),
    }
}

/// Get a human-readable type name for a value.
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Convert an ExecResult to a Value for command substitution return.
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

/// Perform regex match or not-match on two values.
///
/// The left operand is the string to match against.
/// The right operand is the regex pattern.
fn regex_match(left: &Value, right: &Value, negate: bool) -> EvalResult<Value> {
    let text = match left {
        Value::String(s) => s.as_str(),
        _ => {
            return Err(EvalError::TypeError {
                expected: "string",
                got: type_name(left).to_string(),
            })
        }
    };

    let pattern = match right {
        Value::String(s) => s.as_str(),
        _ => {
            return Err(EvalError::TypeError {
                expected: "string (regex pattern)",
                got: type_name(right).to_string(),
            })
        }
    };

    let re = regex::Regex::new(pattern).map_err(|e| EvalError::RegexError(e.to_string()))?;
    let matches = re.is_match(text);

    Ok(Value::Bool(if negate { !matches } else { matches }))
}

/// Convenience function to evaluate an expression with a scope.
///
/// Uses NoOpExecutor, so command substitution will fail.
pub fn eval_expr(expr: &Expr, scope: &mut Scope) -> EvalResult<Value> {
    let mut executor = NoOpExecutor;
    let mut evaluator = Evaluator::new(scope, &mut executor);
    evaluator.eval(expr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::VarSegment;

    // Helper to create a simple variable expression
    fn var_expr(name: &str) -> Expr {
        Expr::VarRef(VarPath::simple(name))
    }

    #[test]
    fn eval_literal_int() {
        let mut scope = Scope::new();
        let expr = Expr::Literal(Value::Int(42));
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Int(42)));
    }

    #[test]
    fn eval_literal_string() {
        let mut scope = Scope::new();
        let expr = Expr::Literal(Value::String("hello".into()));
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::String("hello".into())));
    }

    #[test]
    fn eval_literal_bool() {
        let mut scope = Scope::new();
        assert_eq!(
            eval_expr(&Expr::Literal(Value::Bool(true)), &mut scope),
            Ok(Value::Bool(true))
        );
    }

    #[test]
    fn eval_literal_null() {
        let mut scope = Scope::new();
        assert_eq!(
            eval_expr(&Expr::Literal(Value::Null), &mut scope),
            Ok(Value::Null)
        );
    }

    #[test]
    fn eval_literal_float() {
        let mut scope = Scope::new();
        let expr = Expr::Literal(Value::Float(3.14));
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Float(3.14)));
    }

    #[test]
    fn eval_variable_ref() {
        let mut scope = Scope::new();
        scope.set("X", Value::Int(100));
        assert_eq!(eval_expr(&var_expr("X"), &mut scope), Ok(Value::Int(100)));
    }

    #[test]
    fn eval_undefined_variable() {
        let mut scope = Scope::new();
        let result = eval_expr(&var_expr("MISSING"), &mut scope);
        assert!(matches!(result, Err(EvalError::InvalidPath(_))));
    }

    #[test]
    fn eval_nested_path() {
        let mut scope = Scope::new();
        scope.set(
            "USER",
            Value::Object(vec![
                ("name".into(), Expr::Literal(Value::String("Alice".into()))),
            ]),
        );

        let expr = Expr::VarRef(VarPath {
            segments: vec![
                VarSegment::Field("USER".into()),
                VarSegment::Field("name".into()),
            ],
        });
        assert_eq!(
            eval_expr(&expr, &mut scope),
            Ok(Value::String("Alice".into()))
        );
    }

    #[test]
    fn eval_interpolated_string() {
        let mut scope = Scope::new();
        scope.set("NAME", Value::String("World".into()));

        let expr = Expr::Interpolated(vec![
            StringPart::Literal("Hello, ".into()),
            StringPart::Var(VarPath::simple("NAME")),
            StringPart::Literal("!".into()),
        ]);
        assert_eq!(
            eval_expr(&expr, &mut scope),
            Ok(Value::String("Hello, World!".into()))
        );
    }

    #[test]
    fn eval_interpolated_with_number() {
        let mut scope = Scope::new();
        scope.set("COUNT", Value::Int(42));

        let expr = Expr::Interpolated(vec![
            StringPart::Literal("Count: ".into()),
            StringPart::Var(VarPath::simple("COUNT")),
        ]);
        assert_eq!(
            eval_expr(&expr, &mut scope),
            Ok(Value::String("Count: 42".into()))
        );
    }

    #[test]
    fn eval_and_short_circuit_true() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Bool(true))),
            op: BinaryOp::And,
            right: Box::new(Expr::Literal(Value::Int(42))),
        };
        // true && 42 => 42 (returns right operand)
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Int(42)));
    }

    #[test]
    fn eval_and_short_circuit_false() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Bool(false))),
            op: BinaryOp::And,
            right: Box::new(Expr::Literal(Value::Int(42))),
        };
        // false && 42 => false (returns left operand, short-circuits)
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(false)));
    }

    #[test]
    fn eval_or_short_circuit_true() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Bool(true))),
            op: BinaryOp::Or,
            right: Box::new(Expr::Literal(Value::Int(42))),
        };
        // true || 42 => true (returns left operand, short-circuits)
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_or_short_circuit_false() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Bool(false))),
            op: BinaryOp::Or,
            right: Box::new(Expr::Literal(Value::Int(42))),
        };
        // false || 42 => 42 (returns right operand)
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Int(42)));
    }

    #[test]
    fn eval_equality() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Int(5))),
            op: BinaryOp::Eq,
            right: Box::new(Expr::Literal(Value::Int(5))),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_inequality() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Int(5))),
            op: BinaryOp::NotEq,
            right: Box::new(Expr::Literal(Value::Int(3))),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_less_than() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Int(3))),
            op: BinaryOp::Lt,
            right: Box::new(Expr::Literal(Value::Int(5))),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_greater_than() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Int(5))),
            op: BinaryOp::Gt,
            right: Box::new(Expr::Literal(Value::Int(3))),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_less_than_or_equal() {
        let mut scope = Scope::new();
        let eq = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Int(5))),
            op: BinaryOp::LtEq,
            right: Box::new(Expr::Literal(Value::Int(5))),
        };
        let lt = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Int(3))),
            op: BinaryOp::LtEq,
            right: Box::new(Expr::Literal(Value::Int(5))),
        };
        assert_eq!(eval_expr(&eq, &mut scope), Ok(Value::Bool(true)));
        assert_eq!(eval_expr(&lt, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_greater_than_or_equal() {
        let mut scope = Scope::new();
        let eq = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Int(5))),
            op: BinaryOp::GtEq,
            right: Box::new(Expr::Literal(Value::Int(5))),
        };
        let gt = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Int(7))),
            op: BinaryOp::GtEq,
            right: Box::new(Expr::Literal(Value::Int(5))),
        };
        assert_eq!(eval_expr(&eq, &mut scope), Ok(Value::Bool(true)));
        assert_eq!(eval_expr(&gt, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_string_comparison() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::String("apple".into()))),
            op: BinaryOp::Lt,
            right: Box::new(Expr::Literal(Value::String("banana".into()))),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_mixed_int_float_comparison() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Int(3))),
            op: BinaryOp::Lt,
            right: Box::new(Expr::Literal(Value::Float(3.5))),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_int_float_equality() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Int(5))),
            op: BinaryOp::Eq,
            right: Box::new(Expr::Literal(Value::Float(5.0))),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_type_mismatch_comparison() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Int(5))),
            op: BinaryOp::Lt,
            right: Box::new(Expr::Literal(Value::String("five".into()))),
        };
        assert!(matches!(eval_expr(&expr, &mut scope), Err(EvalError::TypeError { .. })));
    }

    #[test]
    fn eval_array_literal() {
        let mut scope = Scope::new();
        scope.set("X", Value::Int(10));

        let expr = Expr::Literal(Value::Array(vec![
            Expr::Literal(Value::Int(1)),
            Expr::VarRef(VarPath::simple("X")),
            Expr::Literal(Value::Int(3)),
        ]));

        let result = eval_expr(&expr, &mut scope).unwrap();
        if let Value::Array(items) = result {
            assert_eq!(items.len(), 3);
            assert_eq!(items[1], Expr::Literal(Value::Int(10)));
        } else {
            panic!("expected array");
        }
    }

    #[test]
    fn eval_object_literal() {
        let mut scope = Scope::new();
        scope.set("VAL", Value::String("computed".into()));

        let expr = Expr::Literal(Value::Object(vec![
            ("static".into(), Expr::Literal(Value::Int(1))),
            ("dynamic".into(), Expr::VarRef(VarPath::simple("VAL"))),
        ]));

        let result = eval_expr(&expr, &mut scope).unwrap();
        if let Value::Object(fields) = result {
            assert_eq!(fields.len(), 2);
            let dynamic = fields.iter().find(|(k, _)| k == "dynamic").unwrap();
            assert_eq!(dynamic.1, Expr::Literal(Value::String("computed".into())));
        } else {
            panic!("expected object");
        }
    }

    #[test]
    fn is_truthy_values() {
        assert!(!is_truthy(&Value::Null));
        assert!(!is_truthy(&Value::Bool(false)));
        assert!(is_truthy(&Value::Bool(true)));
        assert!(!is_truthy(&Value::Int(0)));
        assert!(is_truthy(&Value::Int(1)));
        assert!(is_truthy(&Value::Int(-1)));
        assert!(!is_truthy(&Value::Float(0.0)));
        assert!(is_truthy(&Value::Float(0.1)));
        assert!(!is_truthy(&Value::String("".into())));
        assert!(is_truthy(&Value::String("x".into())));
        assert!(!is_truthy(&Value::Array(vec![])));
        assert!(is_truthy(&Value::Array(vec![Expr::Literal(Value::Int(1))])));
        assert!(is_truthy(&Value::Object(vec![])));
    }

    #[test]
    fn eval_command_subst_fails_without_executor() {
        use crate::ast::{Command, Pipeline};

        let mut scope = Scope::new();
        let pipeline = Pipeline {
            commands: vec![Command {
                name: "echo".into(),
                args: vec![],
                redirects: vec![],
            }],
            background: false,
        };
        let expr = Expr::CommandSubst(Box::new(pipeline));

        assert!(matches!(
            eval_expr(&expr, &mut scope),
            Err(EvalError::NoExecutor)
        ));
    }

    #[test]
    fn eval_last_result_field() {
        let mut scope = Scope::new();
        scope.set_last_result(ExecResult::failure(42, "test error"));

        // ${?.code}
        let expr = Expr::VarRef(VarPath {
            segments: vec![
                VarSegment::Field("?".into()),
                VarSegment::Field("code".into()),
            ],
        });
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Int(42)));

        // ${?.err}
        let expr = Expr::VarRef(VarPath {
            segments: vec![
                VarSegment::Field("?".into()),
                VarSegment::Field("err".into()),
            ],
        });
        assert_eq!(
            eval_expr(&expr, &mut scope),
            Ok(Value::String("test error".into()))
        );
    }

    #[test]
    fn value_to_string_all_types() {
        assert_eq!(value_to_string(&Value::Null), "null");
        assert_eq!(value_to_string(&Value::Bool(true)), "true");
        assert_eq!(value_to_string(&Value::Int(42)), "42");
        assert_eq!(value_to_string(&Value::Float(3.14)), "3.14");
        assert_eq!(value_to_string(&Value::String("hello".into())), "hello");
    }

    // Additional comprehensive tests

    #[test]
    fn eval_empty_array() {
        let mut scope = Scope::new();
        let expr = Expr::Literal(Value::Array(vec![]));
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Array(vec![])));
    }

    #[test]
    fn eval_empty_object() {
        let mut scope = Scope::new();
        let expr = Expr::Literal(Value::Object(vec![]));
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Object(vec![])));
    }

    #[test]
    fn eval_deeply_nested_object() {
        let mut scope = Scope::new();
        scope.set(
            "ROOT",
            Value::Object(vec![(
                "level1".into(),
                Expr::Literal(Value::Object(vec![(
                    "level2".into(),
                    Expr::Literal(Value::Object(vec![(
                        "level3".into(),
                        Expr::Literal(Value::String("deep".into())),
                    )])),
                )])),
            )]),
        );

        let expr = Expr::VarRef(VarPath {
            segments: vec![
                VarSegment::Field("ROOT".into()),
                VarSegment::Field("level1".into()),
                VarSegment::Field("level2".into()),
                VarSegment::Field("level3".into()),
            ],
        });
        assert_eq!(
            eval_expr(&expr, &mut scope),
            Ok(Value::String("deep".into()))
        );
    }

    #[test]
    fn eval_array_with_variables() {
        let mut scope = Scope::new();
        scope.set("A", Value::Int(1));
        scope.set("B", Value::Int(2));

        let expr = Expr::Literal(Value::Array(vec![
            Expr::VarRef(VarPath::simple("A")),
            Expr::VarRef(VarPath::simple("B")),
        ]));

        if let Ok(Value::Array(items)) = eval_expr(&expr, &mut scope) {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0], Expr::Literal(Value::Int(1)));
            assert_eq!(items[1], Expr::Literal(Value::Int(2)));
        } else {
            panic!("expected array");
        }
    }

    #[test]
    fn eval_negative_int() {
        let mut scope = Scope::new();
        let expr = Expr::Literal(Value::Int(-42));
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Int(-42)));
    }

    #[test]
    fn eval_negative_float() {
        let mut scope = Scope::new();
        let expr = Expr::Literal(Value::Float(-3.14));
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Float(-3.14)));
    }

    #[test]
    fn eval_zero_values() {
        let mut scope = Scope::new();
        assert_eq!(
            eval_expr(&Expr::Literal(Value::Int(0)), &mut scope),
            Ok(Value::Int(0))
        );
        assert_eq!(
            eval_expr(&Expr::Literal(Value::Float(0.0)), &mut scope),
            Ok(Value::Float(0.0))
        );
    }

    #[test]
    fn eval_interpolation_empty_var() {
        let mut scope = Scope::new();
        scope.set("EMPTY", Value::String("".into()));

        let expr = Expr::Interpolated(vec![
            StringPart::Literal("prefix".into()),
            StringPart::Var(VarPath::simple("EMPTY")),
            StringPart::Literal("suffix".into()),
        ]);
        assert_eq!(
            eval_expr(&expr, &mut scope),
            Ok(Value::String("prefixsuffix".into()))
        );
    }

    #[test]
    fn eval_interpolation_nested_path() {
        let mut scope = Scope::new();
        scope.set(
            "USER",
            Value::Object(vec![
                ("name".into(), Expr::Literal(Value::String("Alice".into()))),
            ]),
        );

        let expr = Expr::Interpolated(vec![
            StringPart::Literal("Hello ".into()),
            StringPart::Var(VarPath {
                segments: vec![
                    VarSegment::Field("USER".into()),
                    VarSegment::Field("name".into()),
                ],
            }),
        ]);
        assert_eq!(
            eval_expr(&expr, &mut scope),
            Ok(Value::String("Hello Alice".into()))
        );
    }

    #[test]
    fn eval_chained_and() {
        let mut scope = Scope::new();
        // true && true && 42
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::BinaryOp {
                left: Box::new(Expr::Literal(Value::Bool(true))),
                op: BinaryOp::And,
                right: Box::new(Expr::Literal(Value::Bool(true))),
            }),
            op: BinaryOp::And,
            right: Box::new(Expr::Literal(Value::Int(42))),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Int(42)));
    }

    #[test]
    fn eval_chained_or() {
        let mut scope = Scope::new();
        // false || false || 42
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::BinaryOp {
                left: Box::new(Expr::Literal(Value::Bool(false))),
                op: BinaryOp::Or,
                right: Box::new(Expr::Literal(Value::Bool(false))),
            }),
            op: BinaryOp::Or,
            right: Box::new(Expr::Literal(Value::Int(42))),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Int(42)));
    }

    #[test]
    fn eval_mixed_and_or() {
        let mut scope = Scope::new();
        // true || false && false  (and binds tighter, but here we test explicit tree)
        // This tests: (true || false) && true
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::BinaryOp {
                left: Box::new(Expr::Literal(Value::Bool(true))),
                op: BinaryOp::Or,
                right: Box::new(Expr::Literal(Value::Bool(false))),
            }),
            op: BinaryOp::And,
            right: Box::new(Expr::Literal(Value::Bool(true))),
        };
        // (true || false) = true, true && true = true
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_comparison_with_variables() {
        let mut scope = Scope::new();
        scope.set("X", Value::Int(10));
        scope.set("Y", Value::Int(5));

        let expr = Expr::BinaryOp {
            left: Box::new(var_expr("X")),
            op: BinaryOp::Gt,
            right: Box::new(var_expr("Y")),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_string_equality() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::String("hello".into()))),
            op: BinaryOp::Eq,
            right: Box::new(Expr::Literal(Value::String("hello".into()))),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_string_inequality() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::String("hello".into()))),
            op: BinaryOp::NotEq,
            right: Box::new(Expr::Literal(Value::String("world".into()))),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_null_equality() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Null)),
            op: BinaryOp::Eq,
            right: Box::new(Expr::Literal(Value::Null)),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_null_not_equal_to_int() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Null)),
            op: BinaryOp::Eq,
            right: Box::new(Expr::Literal(Value::Int(0))),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(false)));
    }

    #[test]
    fn eval_array_equality() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Array(vec![
                Expr::Literal(Value::Int(1)),
                Expr::Literal(Value::Int(2)),
            ]))),
            op: BinaryOp::Eq,
            right: Box::new(Expr::Literal(Value::Array(vec![
                Expr::Literal(Value::Int(1)),
                Expr::Literal(Value::Int(2)),
            ]))),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_array_inequality_different_length() {
        let mut scope = Scope::new();
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Array(vec![
                Expr::Literal(Value::Int(1)),
            ]))),
            op: BinaryOp::Eq,
            right: Box::new(Expr::Literal(Value::Array(vec![
                Expr::Literal(Value::Int(1)),
                Expr::Literal(Value::Int(2)),
            ]))),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(false)));
    }

    #[test]
    fn eval_float_comparison_boundary() {
        let mut scope = Scope::new();
        // 1.0 == 1.0 (exact)
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::Literal(Value::Float(1.0))),
            op: BinaryOp::Eq,
            right: Box::new(Expr::Literal(Value::Float(1.0))),
        };
        assert_eq!(eval_expr(&expr, &mut scope), Ok(Value::Bool(true)));
    }

    #[test]
    fn eval_interpolation_with_bool() {
        let mut scope = Scope::new();
        scope.set("FLAG", Value::Bool(true));

        let expr = Expr::Interpolated(vec![
            StringPart::Literal("enabled: ".into()),
            StringPart::Var(VarPath::simple("FLAG")),
        ]);
        assert_eq!(
            eval_expr(&expr, &mut scope),
            Ok(Value::String("enabled: true".into()))
        );
    }

    #[test]
    fn eval_interpolation_with_null() {
        let mut scope = Scope::new();
        scope.set("VAL", Value::Null);

        let expr = Expr::Interpolated(vec![
            StringPart::Literal("value: ".into()),
            StringPart::Var(VarPath::simple("VAL")),
        ]);
        assert_eq!(
            eval_expr(&expr, &mut scope),
            Ok(Value::String("value: null".into()))
        );
    }

    #[test]
    fn eval_format_path_simple() {
        let path = VarPath::simple("X");
        assert_eq!(format_path(&path), "${X}");
    }

    #[test]
    fn eval_format_path_nested() {
        let path = VarPath {
            segments: vec![
                VarSegment::Field("OBJ".into()),
                VarSegment::Field("field".into()),
                VarSegment::Index(0),
            ],
        };
        assert_eq!(format_path(&path), "${OBJ.field[0]}");
    }

    #[test]
    fn type_name_all_types() {
        assert_eq!(type_name(&Value::Null), "null");
        assert_eq!(type_name(&Value::Bool(true)), "bool");
        assert_eq!(type_name(&Value::Int(1)), "int");
        assert_eq!(type_name(&Value::Float(1.0)), "float");
        assert_eq!(type_name(&Value::String("".into())), "string");
        assert_eq!(type_name(&Value::Array(vec![])), "array");
        assert_eq!(type_name(&Value::Object(vec![])), "object");
    }
}
