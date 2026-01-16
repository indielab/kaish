//! Parser for kaish source code.
//!
//! Transforms a token stream from the lexer into an Abstract Syntax Tree.
//! Uses chumsky for parser combinators with good error recovery.

use crate::ast::{
    Arg, Assignment, BinaryOp, Command, Expr, ForLoop, IfStmt, ParamDef, ParamType, Pipeline,
    Program, Redirect, RedirectKind, Stmt, StringPart, ToolDef, Value, VarPath, VarSegment,
};
use crate::lexer::{self, Token};
use chumsky::{input::ValueInput, prelude::*};

/// Span type used throughout the parser.
pub type Span = SimpleSpan;

/// Parse a raw `${...}` string into a VarPath.
///
/// Handles paths like `${VAR}`, `${VAR.field}`, `${VAR[0]}`, `${?.ok}`.
fn parse_varpath(raw: &str) -> VarPath {
    let segments_strs = lexer::parse_var_ref(raw).unwrap_or_default();
    let segments = segments_strs
        .into_iter()
        .map(|s| {
            if s.starts_with('[') && s.ends_with(']') {
                // Index segment like "[0]" - parse the number
                let idx: usize = s[1..s.len() - 1].parse().unwrap_or(0);
                VarSegment::Index(idx)
            } else {
                VarSegment::Field(s)
            }
        })
        .collect();
    VarPath { segments }
}

/// Parse an interpolated string like "Hello ${NAME}!" into parts.
fn parse_interpolated_string(s: &str) -> Vec<StringPart> {
    let mut parts = Vec::new();
    let mut current_text = String::new();
    let mut chars = s.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            // Start of variable reference
            if !current_text.is_empty() {
                parts.push(StringPart::Literal(std::mem::take(&mut current_text)));
            }

            // Consume the '{'
            chars.next();

            // Collect until '}'
            let mut var_content = String::from("${");
            while let Some(c) = chars.next() {
                var_content.push(c);
                if c == '}' {
                    break;
                }
            }

            parts.push(StringPart::Var(parse_varpath(&var_content)));
        } else {
            current_text.push(ch);
        }
    }

    if !current_text.is_empty() {
        parts.push(StringPart::Literal(current_text));
    }

    parts
}

/// Parse error with location and context.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub span: Span,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {:?}", self.message, self.span)
    }
}

impl std::error::Error for ParseError {}

/// Parse kaish source code into a Program AST.
pub fn parse(source: &str) -> Result<Program, Vec<ParseError>> {
    // Tokenize with logos
    let tokens = lexer::tokenize(source).map_err(|errs| {
        errs.into_iter()
            .map(|e| ParseError {
                span: (e.span.start..e.span.end).into(),
                message: format!("lexer error: {}", e.token),
            })
            .collect::<Vec<_>>()
    })?;

    // Convert tokens to (Token, SimpleSpan) pairs
    let tokens: Vec<(Token, Span)> = tokens
        .into_iter()
        .map(|spanned| (spanned.token, (spanned.span.start..spanned.span.end).into()))
        .collect();

    // End-of-input span
    let end_span: Span = (source.len()..source.len()).into();

    // Parse using slice-based input (like nano_rust example)
    let parser = program_parser();
    let result = parser.parse(tokens.as_slice().map(end_span, |(t, s)| (t, s)));

    result.into_result().map_err(|errs| {
        errs.into_iter()
            .map(|e| ParseError {
                span: *e.span(),
                message: e.to_string(),
            })
            .collect()
    })
}

/// Parse a single statement (useful for REPL).
pub fn parse_statement(source: &str) -> Result<Stmt, Vec<ParseError>> {
    let program = parse(source)?;
    program
        .statements
        .into_iter()
        .find(|s| !matches!(s, Stmt::Empty))
        .ok_or_else(|| {
            vec![ParseError {
                span: (0..source.len()).into(),
                message: "empty input".to_string(),
            }]
        })
}

// ═══════════════════════════════════════════════════════════════════════════
// Parser Combinators - generic over input type
// ═══════════════════════════════════════════════════════════════════════════

/// Top-level program parser.
fn program_parser<'tokens, 'src: 'tokens, I>(
) -> impl Parser<'tokens, I, Program, extra::Err<Rich<'tokens, Token, Span>>>
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    statement_parser()
        .repeated()
        .collect::<Vec<_>>()
        .map(|statements| Program { statements })
}

/// Statement parser - dispatches based on leading token.
fn statement_parser<'tokens, I>(
) -> impl Parser<'tokens, I, Stmt, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    recursive(|stmt| {
        let terminator = choice((just(Token::Newline), just(Token::Semi))).repeated();

        choice((
            just(Token::Newline).to(Stmt::Empty),
            assignment_parser().map(Stmt::Assignment),
            tool_def_parser(stmt.clone()).map(Stmt::ToolDef),
            if_parser(stmt.clone()).map(Stmt::If),
            for_parser(stmt).map(Stmt::For),
            pipeline_parser().map(|p| {
                // Unwrap single-command pipelines without background
                if p.commands.len() == 1 && !p.background {
                    // Safe: we just checked len == 1
                    match p.commands.into_iter().next() {
                        Some(cmd) => Stmt::Command(cmd),
                        None => Stmt::Empty, // unreachable but safe
                    }
                } else {
                    Stmt::Pipeline(p)
                }
            }),
        ))
        .boxed()
        .then_ignore(terminator)
    })
}

/// Assignment: `set NAME = value`
fn assignment_parser<'tokens, I>(
) -> impl Parser<'tokens, I, Assignment, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    just(Token::Set)
        .ignore_then(ident_parser())
        .then_ignore(just(Token::Eq))
        .then(expr_parser())
        .map(|(name, value)| Assignment { name, value })
        .labelled("assignment")
        .boxed()
}

/// Tool definition: `tool NAME params { body }`
fn tool_def_parser<'tokens, I, S>(
    stmt: S,
) -> impl Parser<'tokens, I, ToolDef, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
    S: Parser<'tokens, I, Stmt, extra::Err<Rich<'tokens, Token, Span>>> + Clone + 'tokens,
{
    just(Token::Tool)
        .ignore_then(ident_parser())
        .then(param_def_parser().repeated().collect::<Vec<_>>())
        .then_ignore(just(Token::LBrace))
        .then_ignore(just(Token::Newline).repeated())
        .then(
            stmt.repeated()
                .collect::<Vec<_>>()
                .map(|stmts| stmts.into_iter().filter(|s| !matches!(s, Stmt::Empty)).collect()),
        )
        .then_ignore(just(Token::Newline).repeated())
        .then_ignore(just(Token::RBrace))
        .map(|((name, params), body)| ToolDef { name, params, body })
        .labelled("tool definition")
        .boxed()
}

/// Parameter definition: `name: type [= default]`
fn param_def_parser<'tokens, I>(
) -> impl Parser<'tokens, I, ParamDef, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    ident_parser()
        .then_ignore(just(Token::Colon))
        .then(type_parser())
        .then(just(Token::Eq).ignore_then(expr_parser()).or_not())
        .map(|((name, param_type), default)| ParamDef {
            name,
            param_type: Some(param_type),
            default,
        })
        .labelled("parameter")
        .boxed()
}

/// Type keyword parser.
fn type_parser<'tokens, I>(
) -> impl Parser<'tokens, I, ParamType, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    select! {
        Token::TypeString => ParamType::String,
        Token::TypeInt => ParamType::Int,
        Token::TypeFloat => ParamType::Float,
        Token::TypeBool => ParamType::Bool,
        Token::TypeArray => ParamType::Array,
        Token::TypeObject => ParamType::Object,
    }
    .labelled("type")
}

/// If statement: `if COND; then STMTS [else STMTS] fi`
fn if_parser<'tokens, I, S>(
    stmt: S,
) -> impl Parser<'tokens, I, IfStmt, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
    S: Parser<'tokens, I, Stmt, extra::Err<Rich<'tokens, Token, Span>>> + Clone + 'tokens,
{
    just(Token::If)
        .ignore_then(condition_parser())
        .then_ignore(just(Token::Semi).or_not())
        .then_ignore(just(Token::Newline).repeated())
        .then_ignore(just(Token::Then))
        .then_ignore(just(Token::Newline).repeated())
        .then(
            stmt.clone()
                .repeated()
                .collect::<Vec<_>>()
                .map(|stmts| stmts.into_iter().filter(|s| !matches!(s, Stmt::Empty)).collect()),
        )
        .then(
            just(Token::Else)
                .ignore_then(just(Token::Newline).repeated())
                .ignore_then(stmt.repeated().collect::<Vec<_>>())
                .map(|stmts| stmts.into_iter().filter(|s| !matches!(s, Stmt::Empty)).collect())
                .or_not(),
        )
        .then_ignore(just(Token::Fi))
        .map(|((condition, then_branch), else_branch)| IfStmt {
            condition: Box::new(condition),
            then_branch,
            else_branch,
        })
        .labelled("if statement")
        .boxed()
}

/// For loop: `for VAR in ITEMS; do STMTS done`
fn for_parser<'tokens, I, S>(
    stmt: S,
) -> impl Parser<'tokens, I, ForLoop, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
    S: Parser<'tokens, I, Stmt, extra::Err<Rich<'tokens, Token, Span>>> + Clone + 'tokens,
{
    just(Token::For)
        .ignore_then(ident_parser())
        .then_ignore(just(Token::In))
        .then(expr_parser())
        .then_ignore(just(Token::Semi).or_not())
        .then_ignore(just(Token::Newline).repeated())
        .then_ignore(just(Token::Do))
        .then_ignore(just(Token::Newline).repeated())
        .then(
            stmt.repeated()
                .collect::<Vec<_>>()
                .map(|stmts| stmts.into_iter().filter(|s| !matches!(s, Stmt::Empty)).collect()),
        )
        .then_ignore(just(Token::Done))
        .map(|((variable, iterable), body)| ForLoop {
            variable,
            iterable,
            body,
        })
        .labelled("for loop")
        .boxed()
}

/// Pipeline: `cmd | cmd | cmd [&]`
fn pipeline_parser<'tokens, I>(
) -> impl Parser<'tokens, I, Pipeline, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    command_parser()
        .separated_by(just(Token::Pipe))
        .at_least(1)
        .collect::<Vec<_>>()
        .then(just(Token::Amp).or_not())
        .map(|(commands, bg)| Pipeline {
            commands,
            background: bg.is_some(),
        })
        .labelled("pipeline")
        .boxed()
}

/// Command: `name args... [redirects...]`
fn command_parser<'tokens, I>(
) -> impl Parser<'tokens, I, Command, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    ident_parser()
        .then(arg_parser().repeated().collect::<Vec<_>>())
        .then(redirect_parser().repeated().collect::<Vec<_>>())
        .map(|((name, args), redirects)| Command {
            name,
            args,
            redirects,
        })
        .labelled("command")
        .boxed()
}

/// Argument: positional value or `name=value`
fn arg_parser<'tokens, I>(
) -> impl Parser<'tokens, I, Arg, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    ident_parser()
        .then_ignore(just(Token::Eq))
        .then(primary_expr_parser())
        .map(|(key, value)| Arg::Named { key, value })
        .or(primary_expr_parser().map(Arg::Positional))
        .boxed()
}

/// Redirect: `> file`, `>> file`, `< file`, `2> file`, `&> file`
fn redirect_parser<'tokens, I>(
) -> impl Parser<'tokens, I, Redirect, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    let kind = select! {
        Token::GtGt => RedirectKind::StdoutAppend,
        Token::Gt => RedirectKind::StdoutOverwrite,
        Token::Lt => RedirectKind::Stdin,
        Token::Stderr => RedirectKind::Stderr,
        Token::Both => RedirectKind::Both,
    };

    kind.then(primary_expr_parser())
        .map(|(kind, target)| Redirect { kind, target })
        .labelled("redirect")
        .boxed()
}

/// Condition parser: supports comparisons, && and || operators.
///
/// Grammar:
///   condition = or_expr
///   or_expr   = and_expr { "||" and_expr }
///   and_expr  = cmp_expr { "&&" cmp_expr }
///   cmp_expr  = value [ comp_op value ]
fn condition_parser<'tokens, I>(
) -> impl Parser<'tokens, I, Expr, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    let comparison_op = select! {
        Token::EqEq => BinaryOp::Eq,
        Token::NotEq => BinaryOp::NotEq,
        Token::Lt => BinaryOp::Lt,
        Token::Gt => BinaryOp::Gt,
        Token::LtEq => BinaryOp::LtEq,
        Token::GtEq => BinaryOp::GtEq,
    };

    // cmp_expr: value [ comp_op value ]
    let cmp_expr = primary_expr_parser()
        .then(comparison_op.then(primary_expr_parser()).or_not())
        .map(|(left, maybe_op)| match maybe_op {
            Some((op, right)) => Expr::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            },
            None => left,
        });

    // and_expr: cmp_expr { "&&" cmp_expr }
    let and_expr = cmp_expr.clone().foldl(
        just(Token::And).ignore_then(cmp_expr).repeated(),
        |left, right| Expr::BinaryOp {
            left: Box::new(left),
            op: BinaryOp::And,
            right: Box::new(right),
        },
    );

    // or_expr: and_expr { "||" and_expr }
    and_expr
        .clone()
        .foldl(
            just(Token::Or).ignore_then(and_expr).repeated(),
            |left, right| Expr::BinaryOp {
                left: Box::new(left),
                op: BinaryOp::Or,
                right: Box::new(right),
            },
        )
        .labelled("condition")
        .boxed()
}

/// Expression parser - supports && and || binary operators.
fn expr_parser<'tokens, I>(
) -> impl Parser<'tokens, I, Expr, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    // For now, just primary expressions. Can extend for && / || later if needed.
    primary_expr_parser()
}

/// Primary expression: literal, variable reference, command substitution, or bare identifier.
///
/// Uses `recursive` to support nested command substitution like `$(echo $(date))`.
fn primary_expr_parser<'tokens, I>(
) -> impl Parser<'tokens, I, Expr, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    recursive(|expr| {
        choice((
            cmd_subst_parser(expr.clone()),
            var_ref_parser().map(Expr::VarRef),
            interpolated_string_parser(),
            literal_parser().map(Expr::Literal),
            // Bare identifiers become string literals (shell barewords)
            ident_parser().map(|s| Expr::Literal(Value::String(s))),
        ))
        .labelled("expression")
    })
    .boxed()
}

/// Variable reference: `${VAR}` or `${VAR.field}` etc.
fn var_ref_parser<'tokens, I>(
) -> impl Parser<'tokens, I, VarPath, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    select! {
        Token::VarRef(raw) => parse_varpath(&raw),
    }
    .labelled("variable reference")
}

/// Command substitution: `$(pipeline)` - runs a pipeline and returns its result.
///
/// Accepts a recursive expression parser to support nested command substitution.
fn cmd_subst_parser<'tokens, I, E>(
    expr: E,
) -> impl Parser<'tokens, I, Expr, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
    E: Parser<'tokens, I, Expr, extra::Err<Rich<'tokens, Token, Span>>> + Clone,
{
    // Argument parser using the recursive expression parser
    let arg = ident_parser()
        .then_ignore(just(Token::Eq))
        .then(expr.clone())
        .map(|(key, value)| Arg::Named { key, value })
        .or(expr.map(Arg::Positional));

    // Command parser
    let command = ident_parser()
        .then(arg.repeated().collect::<Vec<_>>())
        .map(|(name, args)| Command {
            name,
            args,
            redirects: vec![],
        });

    // Pipeline parser
    let pipeline = command
        .separated_by(just(Token::Pipe))
        .at_least(1)
        .collect::<Vec<_>>()
        .map(|commands| Pipeline {
            commands,
            background: false,
        });

    just(Token::CmdSubstStart)
        .ignore_then(pipeline)
        .then_ignore(just(Token::RParen))
        .map(|pipeline| Expr::CommandSubst(Box::new(pipeline)))
        .labelled("command substitution")
}

/// Interpolated string parser - detects strings with ${} inside.
fn interpolated_string_parser<'tokens, I>(
) -> impl Parser<'tokens, I, Expr, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    select! {
        Token::String(s) => s,
    }
    .map(|s| {
        // Check if string contains interpolation markers
        if s.contains("${") {
            // Parse interpolated parts
            let parts = parse_interpolated_string(&s);
            if parts.len() == 1 {
                if let StringPart::Literal(text) = &parts[0] {
                    return Expr::Literal(Value::String(text.clone()));
                }
            }
            Expr::Interpolated(parts)
        } else {
            Expr::Literal(Value::String(s))
        }
    })
    .labelled("string")
}

/// Literal value parser (excluding strings, which are handled by interpolated_string_parser).
fn literal_parser<'tokens, I>(
) -> impl Parser<'tokens, I, Value, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    choice((
        select! {
            Token::True => Value::Bool(true),
            Token::False => Value::Bool(false),
        },
        select! {
            Token::Int(n) => Value::Int(n),
            Token::Float(f) => Value::Float(f),
        },
        array_parser(),
        object_parser(),
    ))
    .labelled("literal")
    .boxed()
}

/// Array: `[value, value, ...]` - supports nested arrays and objects.
fn array_parser<'tokens, I>(
) -> impl Parser<'tokens, I, Value, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    recursive(|array| {
        // Object parser that can use nested arrays
        let nested_object = recursive(|obj| {
            let value_in_obj = choice((
                select! {
                    Token::True => Expr::Literal(Value::Bool(true)),
                    Token::False => Expr::Literal(Value::Bool(false)),
                    Token::Int(n) => Expr::Literal(Value::Int(n)),
                    Token::Float(f) => Expr::Literal(Value::Float(f)),
                    Token::String(s) => Expr::Literal(Value::String(s)),
                    Token::VarRef(raw) => Expr::VarRef(parse_varpath(&raw)),
                },
                array.clone().map(Expr::Literal),
                obj.map(Expr::Literal),
            ));

            let pair = select! { Token::String(s) => s }
                .then_ignore(just(Token::Colon))
                .then(value_in_obj);

            pair.separated_by(just(Token::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(Token::LBrace), just(Token::RBrace))
                .map(Value::Object)
        });

        let element = choice((
            select! {
                Token::True => Expr::Literal(Value::Bool(true)),
                Token::False => Expr::Literal(Value::Bool(false)),
                Token::Int(n) => Expr::Literal(Value::Int(n)),
                Token::Float(f) => Expr::Literal(Value::Float(f)),
                Token::String(s) => Expr::Literal(Value::String(s)),
                Token::VarRef(raw) => Expr::VarRef(parse_varpath(&raw)),
            },
            array.clone().map(Expr::Literal),
            nested_object.map(Expr::Literal),
        ));

        element
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LBracket), just(Token::RBracket))
            .map(Value::Array)
    })
}

/// Object: `{"key": value, ...}` - supports nested objects and arrays.
fn object_parser<'tokens, I>(
) -> impl Parser<'tokens, I, Value, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    recursive(|obj| {
        // Array parser that can use nested objects
        let nested_array = recursive(|arr| {
            let value_in_arr = choice((
                select! {
                    Token::True => Expr::Literal(Value::Bool(true)),
                    Token::False => Expr::Literal(Value::Bool(false)),
                    Token::Int(n) => Expr::Literal(Value::Int(n)),
                    Token::Float(f) => Expr::Literal(Value::Float(f)),
                    Token::String(s) => Expr::Literal(Value::String(s)),
                    Token::VarRef(raw) => Expr::VarRef(parse_varpath(&raw)),
                },
                arr.map(Expr::Literal),
                obj.clone().map(Expr::Literal),
            ));

            value_in_arr
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(Token::LBracket), just(Token::RBracket))
                .map(Value::Array)
        });

        let value_expr = choice((
            select! {
                Token::True => Expr::Literal(Value::Bool(true)),
                Token::False => Expr::Literal(Value::Bool(false)),
                Token::Int(n) => Expr::Literal(Value::Int(n)),
                Token::Float(f) => Expr::Literal(Value::Float(f)),
                Token::String(s) => Expr::Literal(Value::String(s)),
                Token::VarRef(raw) => Expr::VarRef(parse_varpath(&raw)),
            },
            nested_array.map(Expr::Literal),
            obj.clone().map(Expr::Literal),
        ));

        let pair = select! { Token::String(s) => s }
            .then_ignore(just(Token::Colon))
            .then(value_expr);

        pair.separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LBrace), just(Token::RBrace))
            .map(Value::Object)
            .labelled("object")
    })
    .boxed()
}

/// Identifier parser.
fn ident_parser<'tokens, I>(
) -> impl Parser<'tokens, I, String, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    select! {
        Token::Ident(s) => s,
    }
    .labelled("identifier")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty() {
        let result = parse("");
        assert!(result.is_ok());
        assert_eq!(result.expect("ok").statements.len(), 0);
    }

    #[test]
    fn parse_newlines_only() {
        let result = parse("\n\n\n");
        assert!(result.is_ok());
    }

    #[test]
    fn parse_simple_command() {
        let result = parse("echo");
        assert!(result.is_ok());
        let program = result.expect("ok");
        assert_eq!(program.statements.len(), 1);
        assert!(matches!(&program.statements[0], Stmt::Command(_)));
    }

    #[test]
    fn parse_command_with_string_arg() {
        let result = parse(r#"echo "hello""#);
        assert!(result.is_ok());
        let program = result.expect("ok");
        match &program.statements[0] {
            Stmt::Command(cmd) => assert_eq!(cmd.args.len(), 1),
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn parse_assignment() {
        let result = parse("set X = 5");
        assert!(result.is_ok());
        let program = result.expect("ok");
        assert!(matches!(&program.statements[0], Stmt::Assignment(_)));
    }

    #[test]
    fn parse_pipeline() {
        let result = parse("a | b | c");
        assert!(result.is_ok());
        let program = result.expect("ok");
        match &program.statements[0] {
            Stmt::Pipeline(p) => assert_eq!(p.commands.len(), 3),
            _ => panic!("expected Pipeline"),
        }
    }

    #[test]
    fn parse_background_job() {
        let result = parse("cmd &");
        assert!(result.is_ok());
        let program = result.expect("ok");
        match &program.statements[0] {
            Stmt::Pipeline(p) => assert!(p.background),
            _ => panic!("expected Pipeline with background"),
        }
    }

    #[test]
    fn parse_if_simple() {
        let result = parse("if true; then echo; fi");
        assert!(result.is_ok());
        let program = result.expect("ok");
        assert!(matches!(&program.statements[0], Stmt::If(_)));
    }

    #[test]
    fn parse_if_else() {
        let result = parse("if true; then echo; else echo; fi");
        assert!(result.is_ok());
        let program = result.expect("ok");
        match &program.statements[0] {
            Stmt::If(if_stmt) => assert!(if_stmt.else_branch.is_some()),
            _ => panic!("expected If"),
        }
    }

    #[test]
    fn parse_for_loop() {
        let result = parse("for X in items; do echo; done");
        assert!(result.is_ok());
        let program = result.expect("ok");
        assert!(matches!(&program.statements[0], Stmt::For(_)));
    }

    #[test]
    fn parse_array_literal() {
        let result = parse("cmd [1, 2, 3]");
        assert!(result.is_ok());
    }

    #[test]
    fn parse_object_literal() {
        let result = parse(r#"cmd {"key": "value"}"#);
        assert!(result.is_ok());
    }

    #[test]
    fn parse_named_arg() {
        let result = parse("cmd foo=5");
        assert!(result.is_ok());
        let program = result.expect("ok");
        match &program.statements[0] {
            Stmt::Command(cmd) => {
                assert_eq!(cmd.args.len(), 1);
                assert!(matches!(&cmd.args[0], Arg::Named { .. }));
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn parse_redirect_stdout() {
        let result = parse("cmd > file");
        assert!(result.is_ok());
        let program = result.expect("ok");
        match &program.statements[0] {
            Stmt::Command(cmd) => {
                assert_eq!(cmd.redirects.len(), 1);
                assert!(matches!(cmd.redirects[0].kind, RedirectKind::StdoutOverwrite));
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn parse_var_ref() {
        let result = parse("echo ${VAR}");
        assert!(result.is_ok());
        let program = result.expect("ok");
        match &program.statements[0] {
            Stmt::Command(cmd) => {
                assert_eq!(cmd.args.len(), 1);
                assert!(matches!(&cmd.args[0], Arg::Positional(Expr::VarRef(_))));
            }
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn parse_multiple_statements() {
        let result = parse("a\nb\nc");
        assert!(result.is_ok());
        let program = result.expect("ok");
        let non_empty: Vec<_> = program.statements.iter().filter(|s| !matches!(s, Stmt::Empty)).collect();
        assert_eq!(non_empty.len(), 3);
    }

    #[test]
    fn parse_semicolon_separated() {
        let result = parse("a; b; c");
        assert!(result.is_ok());
        let program = result.expect("ok");
        let non_empty: Vec<_> = program.statements.iter().filter(|s| !matches!(s, Stmt::Empty)).collect();
        assert_eq!(non_empty.len(), 3);
    }

    #[test]
    fn parse_complex_pipeline() {
        let result = parse(r#"cat file | grep pattern="foo" | head count=10"#);
        assert!(result.is_ok());
        let program = result.expect("ok");
        match &program.statements[0] {
            Stmt::Pipeline(p) => assert_eq!(p.commands.len(), 3),
            _ => panic!("expected Pipeline"),
        }
    }

    #[test]
    fn parse_nested_array() {
        let result = parse("cmd [[1, 2], [3, 4]]");
        assert!(result.is_ok());
    }

    #[test]
    fn parse_mixed_args() {
        let result = parse(r#"cmd pos1 key="val" pos2 num=42"#);
        assert!(result.is_ok());
        let program = result.expect("ok");
        match &program.statements[0] {
            Stmt::Command(cmd) => assert_eq!(cmd.args.len(), 4),
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn error_unterminated_string() {
        let result = parse(r#"echo "hello"#);
        assert!(result.is_err());
    }

    #[test]
    fn error_unterminated_var_ref() {
        let result = parse("echo ${VAR");
        assert!(result.is_err());
    }

    #[test]
    fn error_missing_fi() {
        let result = parse("if true; then echo");
        assert!(result.is_err());
    }

    #[test]
    fn error_missing_done() {
        let result = parse("for X in items; do echo");
        assert!(result.is_err());
    }

    #[test]
    fn parse_nested_cmd_subst() {
        // Nested command substitution is supported
        let result = parse("set X = $(echo $(date))").unwrap();
        match &result.statements[0] {
            Stmt::Assignment(a) => {
                assert_eq!(a.name, "X");
                match &a.value {
                    Expr::CommandSubst(outer) => {
                        assert_eq!(outer.commands[0].name, "echo");
                        // The argument should be another command substitution
                        match &outer.commands[0].args[0] {
                            Arg::Positional(Expr::CommandSubst(inner)) => {
                                assert_eq!(inner.commands[0].name, "date");
                            }
                            other => panic!("expected nested cmd subst, got {:?}", other),
                        }
                    }
                    other => panic!("expected cmd subst, got {:?}", other),
                }
            }
            other => panic!("expected assignment, got {:?}", other),
        }
    }

    #[test]
    fn parse_deeply_nested_cmd_subst() {
        // Three levels deep
        let result = parse("set X = $(a $(b $(c)))").unwrap();
        match &result.statements[0] {
            Stmt::Assignment(a) => match &a.value {
                Expr::CommandSubst(level1) => {
                    assert_eq!(level1.commands[0].name, "a");
                    match &level1.commands[0].args[0] {
                        Arg::Positional(Expr::CommandSubst(level2)) => {
                            assert_eq!(level2.commands[0].name, "b");
                            match &level2.commands[0].args[0] {
                                Arg::Positional(Expr::CommandSubst(level3)) => {
                                    assert_eq!(level3.commands[0].name, "c");
                                }
                                other => panic!("expected level3 cmd subst, got {:?}", other),
                            }
                        }
                        other => panic!("expected level2 cmd subst, got {:?}", other),
                    }
                }
                other => panic!("expected cmd subst, got {:?}", other),
            },
            other => panic!("expected assignment, got {:?}", other),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Value Preservation Tests - These test that actual values are captured
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn value_int_preserved() {
        let result = parse("set X = 42").unwrap();
        match &result.statements[0] {
            Stmt::Assignment(a) => {
                assert_eq!(a.name, "X");
                match &a.value {
                    Expr::Literal(Value::Int(n)) => assert_eq!(*n, 42),
                    other => panic!("expected int literal, got {:?}", other),
                }
            }
            other => panic!("expected assignment, got {:?}", other),
        }
    }

    #[test]
    fn value_negative_int_preserved() {
        let result = parse("set X = -99").unwrap();
        match &result.statements[0] {
            Stmt::Assignment(a) => match &a.value {
                Expr::Literal(Value::Int(n)) => assert_eq!(*n, -99),
                other => panic!("expected int, got {:?}", other),
            },
            other => panic!("expected assignment, got {:?}", other),
        }
    }

    #[test]
    fn value_float_preserved() {
        let result = parse("set PI = 3.14").unwrap();
        match &result.statements[0] {
            Stmt::Assignment(a) => match &a.value {
                Expr::Literal(Value::Float(f)) => assert!((*f - 3.14).abs() < 0.001),
                other => panic!("expected float, got {:?}", other),
            },
            other => panic!("expected assignment, got {:?}", other),
        }
    }

    #[test]
    fn value_string_preserved() {
        let result = parse(r#"echo "hello world""#).unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => {
                assert_eq!(cmd.name, "echo");
                match &cmd.args[0] {
                    Arg::Positional(Expr::Literal(Value::String(s))) => {
                        assert_eq!(s, "hello world");
                    }
                    other => panic!("expected string arg, got {:?}", other),
                }
            }
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn value_string_with_escapes_preserved() {
        let result = parse(r#"echo "line1\nline2""#).unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => match &cmd.args[0] {
                Arg::Positional(Expr::Literal(Value::String(s))) => {
                    assert_eq!(s, "line1\nline2");
                }
                other => panic!("expected string, got {:?}", other),
            },
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn value_command_name_preserved() {
        let result = parse("my-command").unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => assert_eq!(cmd.name, "my-command"),
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn value_assignment_name_preserved() {
        let result = parse("set MY_VAR = 1").unwrap();
        match &result.statements[0] {
            Stmt::Assignment(a) => assert_eq!(a.name, "MY_VAR"),
            other => panic!("expected assignment, got {:?}", other),
        }
    }

    #[test]
    fn value_for_variable_preserved() {
        let result = parse("for ITEM in items; do echo; done").unwrap();
        match &result.statements[0] {
            Stmt::For(f) => assert_eq!(f.variable, "ITEM"),
            other => panic!("expected for, got {:?}", other),
        }
    }

    #[test]
    fn value_varref_name_preserved() {
        let result = parse("echo ${MESSAGE}").unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => match &cmd.args[0] {
                Arg::Positional(Expr::VarRef(path)) => {
                    assert_eq!(path.segments.len(), 1);
                    match &path.segments[0] {
                        VarSegment::Field(name) => assert_eq!(name, "MESSAGE"),
                        other => panic!("expected field, got {:?}", other),
                    }
                }
                other => panic!("expected varref, got {:?}", other),
            },
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn value_varref_field_access_preserved() {
        let result = parse("echo ${RESULT.data}").unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => match &cmd.args[0] {
                Arg::Positional(Expr::VarRef(path)) => {
                    assert_eq!(path.segments.len(), 2);
                    match (&path.segments[0], &path.segments[1]) {
                        (VarSegment::Field(a), VarSegment::Field(b)) => {
                            assert_eq!(a, "RESULT");
                            assert_eq!(b, "data");
                        }
                        other => panic!("expected two fields, got {:?}", other),
                    }
                }
                other => panic!("expected varref, got {:?}", other),
            },
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn value_varref_index_preserved() {
        let result = parse("echo ${ITEMS[0]}").unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => match &cmd.args[0] {
                Arg::Positional(Expr::VarRef(path)) => {
                    assert_eq!(path.segments.len(), 2);
                    match &path.segments[1] {
                        VarSegment::Index(i) => assert_eq!(*i, 0),
                        other => panic!("expected index, got {:?}", other),
                    }
                }
                other => panic!("expected varref, got {:?}", other),
            },
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn value_last_result_ref_preserved() {
        let result = parse("echo ${?.ok}").unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => match &cmd.args[0] {
                Arg::Positional(Expr::VarRef(path)) => {
                    assert_eq!(path.segments.len(), 2);
                    match &path.segments[0] {
                        VarSegment::Field(name) => assert_eq!(name, "?"),
                        other => panic!("expected ?, got {:?}", other),
                    }
                }
                other => panic!("expected varref, got {:?}", other),
            },
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn value_object_keys_preserved() {
        let result = parse(r#"cmd {"host": "localhost"}"#).unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => match &cmd.args[0] {
                Arg::Positional(Expr::Literal(Value::Object(pairs))) => {
                    assert_eq!(pairs.len(), 1);
                    assert_eq!(pairs[0].0, "host");
                }
                other => panic!("expected object, got {:?}", other),
            },
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn value_object_values_preserved() {
        let result = parse(r#"cmd {"key": "value"}"#).unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => match &cmd.args[0] {
                Arg::Positional(Expr::Literal(Value::Object(pairs))) => {
                    match &pairs[0].1 {
                        Expr::Literal(Value::String(s)) => assert_eq!(s, "value"),
                        other => panic!("expected string value, got {:?}", other),
                    }
                }
                other => panic!("expected object, got {:?}", other),
            },
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn value_array_ints_preserved() {
        let result = parse("cmd [1, 2, 3]").unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => match &cmd.args[0] {
                Arg::Positional(Expr::Literal(Value::Array(items))) => {
                    assert_eq!(items.len(), 3);
                    match (&items[0], &items[1], &items[2]) {
                        (
                            Expr::Literal(Value::Int(a)),
                            Expr::Literal(Value::Int(b)),
                            Expr::Literal(Value::Int(c)),
                        ) => {
                            assert_eq!(*a, 1);
                            assert_eq!(*b, 2);
                            assert_eq!(*c, 3);
                        }
                        other => panic!("expected three ints, got {:?}", other),
                    }
                }
                other => panic!("expected array, got {:?}", other),
            },
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn value_named_arg_preserved() {
        let result = parse("cmd count=42").unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => {
                assert_eq!(cmd.name, "cmd");
                match &cmd.args[0] {
                    Arg::Named { key, value } => {
                        assert_eq!(key, "count");
                        match value {
                            Expr::Literal(Value::Int(n)) => assert_eq!(*n, 42),
                            other => panic!("expected int, got {:?}", other),
                        }
                    }
                    other => panic!("expected named arg, got {:?}", other),
                }
            }
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn value_tool_def_name_preserved() {
        let result = parse("tool greet name: string { echo }").unwrap();
        match &result.statements[0] {
            Stmt::ToolDef(t) => {
                assert_eq!(t.name, "greet");
                assert_eq!(t.params.len(), 1);
                assert_eq!(t.params[0].name, "name");
            }
            other => panic!("expected tool def, got {:?}", other),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // New Feature Tests - Comparisons, Interpolation, Nested Structures
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_comparison_equals() {
        let result = parse("if ${X} == 5; then echo; fi").unwrap();
        match &result.statements[0] {
            Stmt::If(if_stmt) => match if_stmt.condition.as_ref() {
                Expr::BinaryOp { left, op, right } => {
                    assert!(matches!(left.as_ref(), Expr::VarRef(_)));
                    assert_eq!(*op, BinaryOp::Eq);
                    match right.as_ref() {
                        Expr::Literal(Value::Int(n)) => assert_eq!(*n, 5),
                        other => panic!("expected int, got {:?}", other),
                    }
                }
                other => panic!("expected binary op, got {:?}", other),
            },
            other => panic!("expected if, got {:?}", other),
        }
    }

    #[test]
    fn parse_comparison_not_equals() {
        let result = parse("if ${X} != 0; then echo; fi").unwrap();
        match &result.statements[0] {
            Stmt::If(if_stmt) => match if_stmt.condition.as_ref() {
                Expr::BinaryOp { op, .. } => assert_eq!(*op, BinaryOp::NotEq),
                other => panic!("expected binary op, got {:?}", other),
            },
            other => panic!("expected if, got {:?}", other),
        }
    }

    #[test]
    fn parse_comparison_less_than() {
        let result = parse("if ${COUNT} < 10; then echo; fi").unwrap();
        match &result.statements[0] {
            Stmt::If(if_stmt) => match if_stmt.condition.as_ref() {
                Expr::BinaryOp { op, .. } => assert_eq!(*op, BinaryOp::Lt),
                other => panic!("expected binary op, got {:?}", other),
            },
            other => panic!("expected if, got {:?}", other),
        }
    }

    #[test]
    fn parse_comparison_greater_than() {
        let result = parse("if ${COUNT} > 0; then echo; fi").unwrap();
        match &result.statements[0] {
            Stmt::If(if_stmt) => match if_stmt.condition.as_ref() {
                Expr::BinaryOp { op, .. } => assert_eq!(*op, BinaryOp::Gt),
                other => panic!("expected binary op, got {:?}", other),
            },
            other => panic!("expected if, got {:?}", other),
        }
    }

    #[test]
    fn parse_comparison_less_equal() {
        let result = parse("if ${X} <= 100; then echo; fi").unwrap();
        match &result.statements[0] {
            Stmt::If(if_stmt) => match if_stmt.condition.as_ref() {
                Expr::BinaryOp { op, .. } => assert_eq!(*op, BinaryOp::LtEq),
                other => panic!("expected binary op, got {:?}", other),
            },
            other => panic!("expected if, got {:?}", other),
        }
    }

    #[test]
    fn parse_comparison_greater_equal() {
        let result = parse("if ${X} >= 1; then echo; fi").unwrap();
        match &result.statements[0] {
            Stmt::If(if_stmt) => match if_stmt.condition.as_ref() {
                Expr::BinaryOp { op, .. } => assert_eq!(*op, BinaryOp::GtEq),
                other => panic!("expected binary op, got {:?}", other),
            },
            other => panic!("expected if, got {:?}", other),
        }
    }

    #[test]
    fn parse_string_interpolation() {
        let result = parse(r#"echo "Hello ${NAME}!""#).unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => match &cmd.args[0] {
                Arg::Positional(Expr::Interpolated(parts)) => {
                    assert_eq!(parts.len(), 3);
                    match &parts[0] {
                        StringPart::Literal(s) => assert_eq!(s, "Hello "),
                        other => panic!("expected literal, got {:?}", other),
                    }
                    match &parts[1] {
                        StringPart::Var(path) => {
                            assert_eq!(path.segments.len(), 1);
                            match &path.segments[0] {
                                VarSegment::Field(name) => assert_eq!(name, "NAME"),
                                other => panic!("expected field, got {:?}", other),
                            }
                        }
                        other => panic!("expected var, got {:?}", other),
                    }
                    match &parts[2] {
                        StringPart::Literal(s) => assert_eq!(s, "!"),
                        other => panic!("expected literal, got {:?}", other),
                    }
                }
                other => panic!("expected interpolated, got {:?}", other),
            },
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn parse_string_interpolation_multiple_vars() {
        let result = parse(r#"echo "${FIRST} and ${SECOND}""#).unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => match &cmd.args[0] {
                Arg::Positional(Expr::Interpolated(parts)) => {
                    // ${FIRST} + " and " + ${SECOND} = 3 parts
                    assert_eq!(parts.len(), 3);
                    assert!(matches!(&parts[0], StringPart::Var(_)));
                    assert!(matches!(&parts[1], StringPart::Literal(_)));
                    assert!(matches!(&parts[2], StringPart::Var(_)));
                }
                other => panic!("expected interpolated, got {:?}", other),
            },
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn parse_nested_object() {
        let result = parse(r#"cmd {"config": {"nested": true}}"#).unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => match &cmd.args[0] {
                Arg::Positional(Expr::Literal(Value::Object(pairs))) => {
                    assert_eq!(pairs.len(), 1);
                    assert_eq!(pairs[0].0, "config");
                    match &pairs[0].1 {
                        Expr::Literal(Value::Object(inner)) => {
                            assert_eq!(inner.len(), 1);
                            assert_eq!(inner[0].0, "nested");
                        }
                        other => panic!("expected nested object, got {:?}", other),
                    }
                }
                other => panic!("expected object, got {:?}", other),
            },
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn parse_deeply_nested_object() {
        let result = parse(r#"cmd {"a": {"b": {"c": 1}}}"#).unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => match &cmd.args[0] {
                Arg::Positional(Expr::Literal(Value::Object(pairs))) => {
                    // Just check it parses successfully with nesting
                    assert_eq!(pairs.len(), 1);
                    assert_eq!(pairs[0].0, "a");
                }
                other => panic!("expected object, got {:?}", other),
            },
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn parse_object_in_array() {
        let result = parse(r#"cmd [{"a": 1}, {"b": 2}]"#).unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => match &cmd.args[0] {
                Arg::Positional(Expr::Literal(Value::Array(items))) => {
                    assert_eq!(items.len(), 2);
                    match &items[0] {
                        Expr::Literal(Value::Object(pairs)) => {
                            assert_eq!(pairs[0].0, "a");
                        }
                        other => panic!("expected object, got {:?}", other),
                    }
                    match &items[1] {
                        Expr::Literal(Value::Object(pairs)) => {
                            assert_eq!(pairs[0].0, "b");
                        }
                        other => panic!("expected object, got {:?}", other),
                    }
                }
                other => panic!("expected array, got {:?}", other),
            },
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn parse_array_in_object() {
        let result = parse(r#"cmd {"items": [1, 2, 3]}"#).unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => match &cmd.args[0] {
                Arg::Positional(Expr::Literal(Value::Object(pairs))) => {
                    assert_eq!(pairs.len(), 1);
                    assert_eq!(pairs[0].0, "items");
                    match &pairs[0].1 {
                        Expr::Literal(Value::Array(items)) => {
                            assert_eq!(items.len(), 3);
                        }
                        other => panic!("expected array, got {:?}", other),
                    }
                }
                other => panic!("expected object, got {:?}", other),
            },
            other => panic!("expected command, got {:?}", other),
        }
    }

    #[test]
    fn parse_empty_tool_body() {
        let result = parse("tool empty { }").unwrap();
        match &result.statements[0] {
            Stmt::ToolDef(t) => {
                assert_eq!(t.name, "empty");
                assert_eq!(t.params.len(), 0);
                assert_eq!(t.body.len(), 0);
            }
            other => panic!("expected tool def, got {:?}", other),
        }
    }

    #[test]
    fn parse_tool_with_default_param() {
        let result = parse("tool greet name: string = \"World\" { echo }").unwrap();
        match &result.statements[0] {
            Stmt::ToolDef(t) => {
                assert_eq!(t.name, "greet");
                assert_eq!(t.params.len(), 1);
                assert_eq!(t.params[0].name, "name");
                assert!(t.params[0].default.is_some());
                match &t.params[0].default {
                    Some(Expr::Literal(Value::String(s))) => assert_eq!(s, "World"),
                    other => panic!("expected string default, got {:?}", other),
                }
            }
            other => panic!("expected tool def, got {:?}", other),
        }
    }

    #[test]
    fn parse_comparison_string_values() {
        let result = parse(r#"if ${STATUS} == "ok"; then echo; fi"#).unwrap();
        match &result.statements[0] {
            Stmt::If(if_stmt) => match if_stmt.condition.as_ref() {
                Expr::BinaryOp { left, op, right } => {
                    assert!(matches!(left.as_ref(), Expr::VarRef(_)));
                    assert_eq!(*op, BinaryOp::Eq);
                    match right.as_ref() {
                        Expr::Literal(Value::String(s)) => assert_eq!(s, "ok"),
                        other => panic!("expected string, got {:?}", other),
                    }
                }
                other => panic!("expected binary op, got {:?}", other),
            },
            other => panic!("expected if, got {:?}", other),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Command Substitution Tests
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_cmd_subst_simple() {
        let result = parse("set X = $(echo)").unwrap();
        match &result.statements[0] {
            Stmt::Assignment(a) => {
                assert_eq!(a.name, "X");
                match &a.value {
                    Expr::CommandSubst(pipeline) => {
                        assert_eq!(pipeline.commands.len(), 1);
                        assert_eq!(pipeline.commands[0].name, "echo");
                    }
                    other => panic!("expected command subst, got {:?}", other),
                }
            }
            other => panic!("expected assignment, got {:?}", other),
        }
    }

    #[test]
    fn parse_cmd_subst_with_args() {
        let result = parse(r#"set X = $(fetch url="http://example.com")"#).unwrap();
        match &result.statements[0] {
            Stmt::Assignment(a) => match &a.value {
                Expr::CommandSubst(pipeline) => {
                    assert_eq!(pipeline.commands[0].name, "fetch");
                    assert_eq!(pipeline.commands[0].args.len(), 1);
                    match &pipeline.commands[0].args[0] {
                        Arg::Named { key, .. } => assert_eq!(key, "url"),
                        other => panic!("expected named arg, got {:?}", other),
                    }
                }
                other => panic!("expected command subst, got {:?}", other),
            },
            other => panic!("expected assignment, got {:?}", other),
        }
    }

    #[test]
    fn parse_cmd_subst_pipeline() {
        let result = parse("set X = $(cat file | grep pattern)").unwrap();
        match &result.statements[0] {
            Stmt::Assignment(a) => match &a.value {
                Expr::CommandSubst(pipeline) => {
                    assert_eq!(pipeline.commands.len(), 2);
                    assert_eq!(pipeline.commands[0].name, "cat");
                    assert_eq!(pipeline.commands[1].name, "grep");
                }
                other => panic!("expected command subst, got {:?}", other),
            },
            other => panic!("expected assignment, got {:?}", other),
        }
    }

    #[test]
    fn parse_cmd_subst_in_condition() {
        let result = parse("if $(validate); then echo; fi").unwrap();
        match &result.statements[0] {
            Stmt::If(if_stmt) => match if_stmt.condition.as_ref() {
                Expr::CommandSubst(pipeline) => {
                    assert_eq!(pipeline.commands[0].name, "validate");
                }
                other => panic!("expected command subst, got {:?}", other),
            },
            other => panic!("expected if, got {:?}", other),
        }
    }

    #[test]
    fn parse_cmd_subst_in_command_arg() {
        let result = parse("echo $(whoami)").unwrap();
        match &result.statements[0] {
            Stmt::Command(cmd) => {
                assert_eq!(cmd.name, "echo");
                match &cmd.args[0] {
                    Arg::Positional(Expr::CommandSubst(pipeline)) => {
                        assert_eq!(pipeline.commands[0].name, "whoami");
                    }
                    other => panic!("expected command subst, got {:?}", other),
                }
            }
            other => panic!("expected command, got {:?}", other),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Logical Operator Tests (&&, ||)
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_condition_and() {
        let result = parse("if $(check-a) && $(check-b); then echo; fi").unwrap();
        match &result.statements[0] {
            Stmt::If(if_stmt) => match if_stmt.condition.as_ref() {
                Expr::BinaryOp { left, op, right } => {
                    assert_eq!(*op, BinaryOp::And);
                    assert!(matches!(left.as_ref(), Expr::CommandSubst(_)));
                    assert!(matches!(right.as_ref(), Expr::CommandSubst(_)));
                }
                other => panic!("expected binary op, got {:?}", other),
            },
            other => panic!("expected if, got {:?}", other),
        }
    }

    #[test]
    fn parse_condition_or() {
        let result = parse("if $(try-a) || $(try-b); then echo; fi").unwrap();
        match &result.statements[0] {
            Stmt::If(if_stmt) => match if_stmt.condition.as_ref() {
                Expr::BinaryOp { left, op, right } => {
                    assert_eq!(*op, BinaryOp::Or);
                    assert!(matches!(left.as_ref(), Expr::CommandSubst(_)));
                    assert!(matches!(right.as_ref(), Expr::CommandSubst(_)));
                }
                other => panic!("expected binary op, got {:?}", other),
            },
            other => panic!("expected if, got {:?}", other),
        }
    }

    #[test]
    fn parse_condition_and_or_precedence() {
        // a && b || c should parse as (a && b) || c
        let result = parse("if $(a) && $(b) || $(c); then echo; fi").unwrap();
        match &result.statements[0] {
            Stmt::If(if_stmt) => match if_stmt.condition.as_ref() {
                Expr::BinaryOp { left, op, right } => {
                    // Top level should be ||
                    assert_eq!(*op, BinaryOp::Or);
                    // Left side should be && expression
                    match left.as_ref() {
                        Expr::BinaryOp { op: inner_op, .. } => {
                            assert_eq!(*inner_op, BinaryOp::And);
                        }
                        other => panic!("expected binary op (&&), got {:?}", other),
                    }
                    // Right side should be $(c)
                    assert!(matches!(right.as_ref(), Expr::CommandSubst(_)));
                }
                other => panic!("expected binary op, got {:?}", other),
            },
            other => panic!("expected if, got {:?}", other),
        }
    }

    #[test]
    fn parse_condition_multiple_and() {
        let result = parse("if $(a) && $(b) && $(c); then echo; fi").unwrap();
        match &result.statements[0] {
            Stmt::If(if_stmt) => match if_stmt.condition.as_ref() {
                Expr::BinaryOp { left, op, .. } => {
                    assert_eq!(*op, BinaryOp::And);
                    // Left side should also be &&
                    match left.as_ref() {
                        Expr::BinaryOp { op: inner_op, .. } => {
                            assert_eq!(*inner_op, BinaryOp::And);
                        }
                        other => panic!("expected binary op, got {:?}", other),
                    }
                }
                other => panic!("expected binary op, got {:?}", other),
            },
            other => panic!("expected if, got {:?}", other),
        }
    }

    #[test]
    fn parse_condition_mixed_comparison_and_logical() {
        // ${X} == 5 && ${Y} > 0
        let result = parse("if ${X} == 5 && ${Y} > 0; then echo; fi").unwrap();
        match &result.statements[0] {
            Stmt::If(if_stmt) => match if_stmt.condition.as_ref() {
                Expr::BinaryOp { left, op, right } => {
                    assert_eq!(*op, BinaryOp::And);
                    // Left: ${X} == 5
                    match left.as_ref() {
                        Expr::BinaryOp { op: left_op, .. } => {
                            assert_eq!(*left_op, BinaryOp::Eq);
                        }
                        other => panic!("expected comparison, got {:?}", other),
                    }
                    // Right: ${Y} > 0
                    match right.as_ref() {
                        Expr::BinaryOp { op: right_op, .. } => {
                            assert_eq!(*right_op, BinaryOp::Gt);
                        }
                        other => panic!("expected comparison, got {:?}", other),
                    }
                }
                other => panic!("expected binary op, got {:?}", other),
            },
            other => panic!("expected if, got {:?}", other),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Integration Tests - Complete Scripts
    // ═══════════════════════════════════════════════════════════════════════════

    /// Level 1: Linear script using core features
    #[test]
    fn script_level1_linear() {
        let script = r#"
set NAME = "kaish"
set VERSION = 1
set CONFIG = {"debug": true, "timeout": 30}
set ITEMS = ["alpha", "beta", "gamma"]

echo "Starting ${NAME} v${VERSION}"
cat "README.md" | grep pattern="install" | head count=5
fetch url="https://api.example.com/status" timeout=${CONFIG.timeout} > "/scratch/status.json"
echo "First item: ${ITEMS[0]}"
"#;
        let result = parse(script).unwrap();
        let stmts: Vec<_> = result.statements.iter()
            .filter(|s| !matches!(s, Stmt::Empty))
            .collect();

        assert_eq!(stmts.len(), 8);
        assert!(matches!(stmts[0], Stmt::Assignment(_)));  // set NAME
        assert!(matches!(stmts[1], Stmt::Assignment(_)));  // set VERSION
        assert!(matches!(stmts[2], Stmt::Assignment(_)));  // set CONFIG
        assert!(matches!(stmts[3], Stmt::Assignment(_)));  // set ITEMS
        assert!(matches!(stmts[4], Stmt::Command(_)));     // echo
        assert!(matches!(stmts[5], Stmt::Pipeline(_)));    // cat | grep | head
        assert!(matches!(stmts[6], Stmt::Command(_)));     // fetch (with redirect)
        assert!(matches!(stmts[7], Stmt::Command(_)));     // echo
    }

    /// Level 2: Script with conditionals
    #[test]
    fn script_level2_branching() {
        let script = r#"
set RESULT = $(validate "input.json")

if ${RESULT.ok}; then
    echo "Validation passed"
    process "input.json" > "output.json"
else
    echo "Validation failed: ${RESULT.err}"
fi

if ${COUNT} > 0 && ${COUNT} <= 100; then
    echo "Count in valid range"
fi

if $(check-network) || $(check-cache); then
    fetch url=${URL}
fi
"#;
        let result = parse(script).unwrap();
        let stmts: Vec<_> = result.statements.iter()
            .filter(|s| !matches!(s, Stmt::Empty))
            .collect();

        assert_eq!(stmts.len(), 4);

        // First: assignment with command substitution
        match stmts[0] {
            Stmt::Assignment(a) => {
                assert_eq!(a.name, "RESULT");
                assert!(matches!(&a.value, Expr::CommandSubst(_)));
            }
            other => panic!("expected assignment, got {:?}", other),
        }

        // Second: if/else
        match stmts[1] {
            Stmt::If(if_stmt) => {
                assert_eq!(if_stmt.then_branch.len(), 2);
                assert!(if_stmt.else_branch.is_some());
                assert_eq!(if_stmt.else_branch.as_ref().unwrap().len(), 1);
            }
            other => panic!("expected if, got {:?}", other),
        }

        // Third: if with && condition
        match stmts[2] {
            Stmt::If(if_stmt) => {
                match if_stmt.condition.as_ref() {
                    Expr::BinaryOp { op, .. } => assert_eq!(*op, BinaryOp::And),
                    other => panic!("expected && condition, got {:?}", other),
                }
            }
            other => panic!("expected if, got {:?}", other),
        }

        // Fourth: if with || of command substitutions
        match stmts[3] {
            Stmt::If(if_stmt) => {
                match if_stmt.condition.as_ref() {
                    Expr::BinaryOp { op, left, right } => {
                        assert_eq!(*op, BinaryOp::Or);
                        assert!(matches!(left.as_ref(), Expr::CommandSubst(_)));
                        assert!(matches!(right.as_ref(), Expr::CommandSubst(_)));
                    }
                    other => panic!("expected || condition, got {:?}", other),
                }
            }
            other => panic!("expected if, got {:?}", other),
        }
    }

    /// Level 3: Script with loops and tool definitions
    #[test]
    fn script_level3_loops_and_tools() {
        let script = r#"
tool greet name: string prefix: string = "Hello" {
    echo "${prefix}, ${name}!"
}

tool fetch-all urls: array {
    for URL in ${urls}; do
        fetch url=${URL}
    done
}

set USERS = ["alice", "bob", "charlie"]

for USER in ${USERS}; do
    greet name=${USER}
    if ${USER} == "bob"; then
        echo "Found Bob!"
    fi
done

long-running-task &
"#;
        let result = parse(script).unwrap();
        let stmts: Vec<_> = result.statements.iter()
            .filter(|s| !matches!(s, Stmt::Empty))
            .collect();

        assert_eq!(stmts.len(), 5);

        // First tool def
        match stmts[0] {
            Stmt::ToolDef(t) => {
                assert_eq!(t.name, "greet");
                assert_eq!(t.params.len(), 2);
                assert_eq!(t.params[0].name, "name");
                assert!(t.params[0].default.is_none());
                assert_eq!(t.params[1].name, "prefix");
                assert!(t.params[1].default.is_some());
            }
            other => panic!("expected tool def, got {:?}", other),
        }

        // Second tool def with nested for loop
        match stmts[1] {
            Stmt::ToolDef(t) => {
                assert_eq!(t.name, "fetch-all");
                assert_eq!(t.body.len(), 1);
                assert!(matches!(&t.body[0], Stmt::For(_)));
            }
            other => panic!("expected tool def, got {:?}", other),
        }

        // Assignment
        assert!(matches!(stmts[2], Stmt::Assignment(_)));

        // For loop with nested if
        match stmts[3] {
            Stmt::For(f) => {
                assert_eq!(f.variable, "USER");
                assert_eq!(f.body.len(), 2);
                assert!(matches!(&f.body[0], Stmt::Command(_)));
                assert!(matches!(&f.body[1], Stmt::If(_)));
            }
            other => panic!("expected for loop, got {:?}", other),
        }

        // Background job
        match stmts[4] {
            Stmt::Pipeline(p) => {
                assert!(p.background);
                assert_eq!(p.commands[0].name, "long-running-task");
            }
            other => panic!("expected pipeline (background), got {:?}", other),
        }
    }

    /// Level 4: Complex nested structures
    #[test]
    fn script_level4_complex_nesting() {
        let script = r#"
set SERVERS = [{"host": "prod-1", "port": 8080, "tags": ["primary", "us-west"]}, {"host": "prod-2", "port": 8080, "tags": ["replica", "us-east"]}]

set RESULT = $(cat "config.json" | jq query=".servers" | validate schema="server-schema.json")

if $(ping host=${HOST}) && ${RESULT.ok}; then
    for SERVER in ${SERVERS}; do
        deploy target=${SERVER.host} port=${SERVER.port}
        if ${?.code} != 0; then
            notify channel="ops" message="Deploy failed"
        fi
    done
fi
"#;
        let result = parse(script).unwrap();
        let stmts: Vec<_> = result.statements.iter()
            .filter(|s| !matches!(s, Stmt::Empty))
            .collect();

        assert_eq!(stmts.len(), 3);

        // Complex array of objects
        match stmts[0] {
            Stmt::Assignment(a) => {
                assert_eq!(a.name, "SERVERS");
                match &a.value {
                    Expr::Literal(Value::Array(items)) => {
                        assert_eq!(items.len(), 2);
                        // First object has nested array
                        match &items[0] {
                            Expr::Literal(Value::Object(pairs)) => {
                                assert_eq!(pairs.len(), 3);
                                assert_eq!(pairs[2].0, "tags");
                            }
                            other => panic!("expected object, got {:?}", other),
                        }
                    }
                    other => panic!("expected array, got {:?}", other),
                }
            }
            other => panic!("expected assignment, got {:?}", other),
        }

        // Command substitution with pipeline
        match stmts[1] {
            Stmt::Assignment(a) => {
                assert_eq!(a.name, "RESULT");
                match &a.value {
                    Expr::CommandSubst(pipeline) => {
                        assert_eq!(pipeline.commands.len(), 3);
                    }
                    other => panic!("expected command subst, got {:?}", other),
                }
            }
            other => panic!("expected assignment, got {:?}", other),
        }

        // If with && condition, containing for loop with nested if
        match stmts[2] {
            Stmt::If(if_stmt) => {
                match if_stmt.condition.as_ref() {
                    Expr::BinaryOp { op, .. } => assert_eq!(*op, BinaryOp::And),
                    other => panic!("expected && condition, got {:?}", other),
                }
                assert_eq!(if_stmt.then_branch.len(), 1);
                match &if_stmt.then_branch[0] {
                    Stmt::For(f) => {
                        assert_eq!(f.body.len(), 2);
                        assert!(matches!(&f.body[1], Stmt::If(_)));
                    }
                    other => panic!("expected for in if body, got {:?}", other),
                }
            }
            other => panic!("expected if, got {:?}", other),
        }
    }

    /// Level 5: Edge cases and parser stress test
    #[test]
    fn script_level5_edge_cases() {
        let script = r#"
set EMPTY_ARRAY = []
set EMPTY_OBJECT = {}
set NESTED = [[1, 2], [3, 4]]
set DEEP = {"a": {"b": {"c": {"d": 1}}}}

echo ""
echo "quotes: \"nested\" here"
echo "escapes: \n\t\r\\"
echo "unicode: \u2764"

set X = -99999
set Y = 3.14159265358979
set Z = -0.001

cmd a=1 b="two" c=true d=false e=null f=[1,2,3] g={"k":"v"}

if true; then
    if false; then
        echo "inner"
    else
        echo "else"
    fi
fi

for I in ${EMPTY_ARRAY}; do
    echo "never"
done

tool no-params {
    echo "no params"
}

tool all-types a: string b: int c: float d: bool e: array f: object {
    echo "typed"
}

a | b | c | d | e &
cmd 2> "errors.log"
cmd &> "all.log"
cmd >> "append.log"
cmd < "input.txt"
"#;
        let result = parse(script).unwrap();
        let stmts: Vec<_> = result.statements.iter()
            .filter(|s| !matches!(s, Stmt::Empty))
            .collect();

        // Just verify it parses without error - this is the stress test
        assert!(stmts.len() >= 15, "expected many statements, got {}", stmts.len());

        // Verify specific edge cases

        // Empty array
        match stmts[0] {
            Stmt::Assignment(a) => match &a.value {
                Expr::Literal(Value::Array(items)) => assert_eq!(items.len(), 0),
                other => panic!("expected empty array, got {:?}", other),
            },
            other => panic!("expected assignment, got {:?}", other),
        }

        // Empty object
        match stmts[1] {
            Stmt::Assignment(a) => match &a.value {
                Expr::Literal(Value::Object(pairs)) => assert_eq!(pairs.len(), 0),
                other => panic!("expected empty object, got {:?}", other),
            },
            other => panic!("expected assignment, got {:?}", other),
        }

        // Nested arrays
        match stmts[2] {
            Stmt::Assignment(a) => match &a.value {
                Expr::Literal(Value::Array(items)) => {
                    assert_eq!(items.len(), 2);
                    assert!(matches!(&items[0], Expr::Literal(Value::Array(_))));
                }
                other => panic!("expected nested array, got {:?}", other),
            },
            other => panic!("expected assignment, got {:?}", other),
        }

        // Deeply nested object
        match stmts[3] {
            Stmt::Assignment(a) => match &a.value {
                Expr::Literal(Value::Object(pairs)) => {
                    assert_eq!(pairs.len(), 1);
                    assert_eq!(pairs[0].0, "a");
                }
                other => panic!("expected nested object, got {:?}", other),
            },
            other => panic!("expected assignment, got {:?}", other),
        }

        // Background pipeline
        let bg_stmt = stmts.iter().find(|s| matches!(s, Stmt::Pipeline(p) if p.background));
        assert!(bg_stmt.is_some(), "expected background pipeline");

        match bg_stmt.unwrap() {
            Stmt::Pipeline(p) => {
                assert_eq!(p.commands.len(), 5);
                assert!(p.background);
            }
            _ => unreachable!(),
        }
    }
}
