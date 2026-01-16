//! kaish-kernel (核): The core of 会sh.
//!
//! This crate provides:
//!
//! - **Lexer**: Tokenizes kaish source code using logos
//! - **Parser**: Builds AST from tokens using chumsky
//! - **AST**: Type definitions for the abstract syntax tree
//!
//! Future layers will add:
//! - Interpreter and expression evaluation
//! - VFS (virtual filesystem) with mount points
//! - Tool registry and builtins
//! - Job scheduler for pipelines and background tasks

pub mod ast;
pub mod lexer;
pub mod parser;
