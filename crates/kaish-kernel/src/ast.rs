//! Abstract Syntax Tree types for kaish.
//!
//! These types represent parsed kaish source code. The parser produces an AST,
//! which is then interpreted by the runtime.

use std::fmt;

/// A complete kaish program is a sequence of statements.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub statements: Vec<Stmt>,
}

/// A single statement in kaish.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// Variable assignment: `set X = value`
    Assignment(Assignment),
    /// Simple command: `tool arg1 arg2`
    Command(Command),
    /// Pipeline: `a | b | c`
    Pipeline(Pipeline),
    /// Conditional: `if cond; then ...; fi`
    If(IfStmt),
    /// Loop: `for X in items; do ...; done`
    For(ForLoop),
    /// Tool definition: `tool name(params) { body }`
    ToolDef(ToolDef),
    /// Empty statement (newline or semicolon only)
    Empty,
}

/// Variable assignment: `set NAME = value`
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub name: String,
    pub value: Expr,
}

/// A command invocation with arguments and redirections.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub name: String,
    pub args: Vec<Arg>,
    pub redirects: Vec<Redirect>,
}

/// A pipeline of commands connected by pipes.
#[derive(Debug, Clone, PartialEq)]
pub struct Pipeline {
    pub commands: Vec<Command>,
    pub background: bool,
}

/// Conditional statement.
#[derive(Debug, Clone, PartialEq)]
pub struct IfStmt {
    pub condition: Box<Expr>,
    pub then_branch: Vec<Stmt>,
    pub else_branch: Option<Vec<Stmt>>,
}

/// For loop over items.
#[derive(Debug, Clone, PartialEq)]
pub struct ForLoop {
    pub variable: String,
    pub iterable: Expr,
    pub body: Vec<Stmt>,
}

/// User-defined tool.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub params: Vec<ParamDef>,
    pub body: Vec<Stmt>,
}

/// Parameter definition for a tool.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamDef {
    pub name: String,
    pub param_type: Option<ParamType>,
    pub default: Option<Expr>,
}

/// Parameter type annotation.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamType {
    String,
    Int,
    Float,
    Bool,
    Array,
    Object,
}

/// A command argument (positional or named).
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    /// Positional argument: `value`
    Positional(Expr),
    /// Named argument: `key=value`
    Named { key: String, value: Expr },
    /// Short flag: `-l`, `-v` (boolean flag)
    ShortFlag(String),
    /// Long flag: `--force`, `--verbose` (boolean flag)
    LongFlag(String),
}

/// I/O redirection.
#[derive(Debug, Clone, PartialEq)]
pub struct Redirect {
    pub kind: RedirectKind,
    pub target: Expr,
}

/// Type of redirection.
#[derive(Debug, Clone, PartialEq)]
pub enum RedirectKind {
    /// `>` stdout to file (overwrite)
    StdoutOverwrite,
    /// `>>` stdout to file (append)
    StdoutAppend,
    /// `<` stdin from file
    Stdin,
    /// `2>` stderr to file
    Stderr,
    /// `&>` both stdout and stderr to file
    Both,
}

/// An expression that evaluates to a value.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// Literal value
    Literal(Value),
    /// Variable reference: `${VAR}` or `${VAR.field}`
    VarRef(VarPath),
    /// String with interpolation: `"hello ${NAME}"`
    Interpolated(Vec<StringPart>),
    /// Binary operation: `a && b`, `a || b`
    BinaryOp {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
    /// Command substitution: `$(pipeline)` - runs a pipeline and returns its result
    CommandSubst(Box<Pipeline>),
}

/// A literal value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Array(Vec<Expr>),
    Object(Vec<(String, Expr)>),
}

/// Variable reference path: `${VAR.field[0].nested}`
#[derive(Debug, Clone, PartialEq)]
pub struct VarPath {
    pub segments: Vec<VarSegment>,
}

impl VarPath {
    /// Create a simple variable reference with just a name.
    pub fn simple(name: impl Into<String>) -> Self {
        Self {
            segments: vec![VarSegment::Field(name.into())],
        }
    }
}

/// A segment in a variable path.
#[derive(Debug, Clone, PartialEq)]
pub enum VarSegment {
    /// Field access: `.field` or initial name
    Field(String),
    /// Array index: `[0]`
    Index(usize),
}

/// Part of an interpolated string.
#[derive(Debug, Clone, PartialEq)]
pub enum StringPart {
    /// Literal text
    Literal(String),
    /// Variable interpolation
    Var(VarPath),
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    /// `&&` - logical and (short-circuit)
    And,
    /// `||` - logical or (short-circuit)
    Or,
    /// `==` - equality
    Eq,
    /// `!=` - inequality
    NotEq,
    /// `<` - less than
    Lt,
    /// `>` - greater than
    Gt,
    /// `<=` - less than or equal
    LtEq,
    /// `>=` - greater than or equal
    GtEq,
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinaryOp::And => write!(f, "&&"),
            BinaryOp::Or => write!(f, "||"),
            BinaryOp::Eq => write!(f, "=="),
            BinaryOp::NotEq => write!(f, "!="),
            BinaryOp::Lt => write!(f, "<"),
            BinaryOp::Gt => write!(f, ">"),
            BinaryOp::LtEq => write!(f, "<="),
            BinaryOp::GtEq => write!(f, ">="),
        }
    }
}

impl fmt::Display for RedirectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RedirectKind::StdoutOverwrite => write!(f, ">"),
            RedirectKind::StdoutAppend => write!(f, ">>"),
            RedirectKind::Stdin => write!(f, "<"),
            RedirectKind::Stderr => write!(f, "2>"),
            RedirectKind::Both => write!(f, "&>"),
        }
    }
}
