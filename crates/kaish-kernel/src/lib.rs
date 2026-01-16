//! kaish-kernel (核): The core of 会sh.
//!
//! This crate provides:
//!
//! - **Lexer**: Tokenizes kaish source code using logos
//! - **Parser**: Builds AST from tokens using chumsky
//! - **AST**: Type definitions for the abstract syntax tree
//! - **Interpreter**: Expression evaluation, scopes, and the `$?` result type
//! - **VFS**: Virtual filesystem with mount points
//! - **Tools**: Tool trait, registry, and builtin commands
//!
//! Future layers will add:
//! - Job scheduler for pipelines and background tasks

pub mod ast;
pub mod interpreter;
pub mod lexer;
pub mod parser;
pub mod tools;
pub mod vfs;
