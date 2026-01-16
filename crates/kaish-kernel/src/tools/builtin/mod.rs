//! Built-in tools for kaish.
//!
//! These tools are always available and provide core functionality.

mod cat;
mod cd;
mod echo;
mod ls;
mod mkdir;
mod pwd;
mod rm;
mod write;

use super::ToolRegistry;

/// Register all built-in tools with the registry.
pub fn register_builtins(registry: &mut ToolRegistry) {
    registry.register(echo::Echo);
    registry.register(cat::Cat);
    registry.register(ls::Ls);
    registry.register(pwd::Pwd);
    registry.register(cd::Cd);
    registry.register(mkdir::Mkdir);
    registry.register(write::Write);
    registry.register(rm::Rm);
}
