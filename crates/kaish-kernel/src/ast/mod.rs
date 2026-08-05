//! Abstract Syntax Tree types for kaish.
//!
//! This module provides:
//! - AST type definitions (`types` module, re-exported at this level)
//! - S-expression formatter for test snapshots (`sexpr` module)
//! - The statement plan: an unexpanded rendering plus the commands a
//!   statement would run (`plan` module)

mod types;
pub mod plan;
pub mod sexpr;

pub use types::*;
