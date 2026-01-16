//! Execution context for tools.

use std::path::PathBuf;
use std::sync::Arc;

use crate::interpreter::Scope;
use crate::vfs::VfsRouter;

/// Execution context passed to tools.
///
/// Provides access to the VFS, scope, and other kernel state.
pub struct ExecContext {
    /// Virtual filesystem.
    pub vfs: Arc<VfsRouter>,
    /// Variable scope.
    pub scope: Scope,
    /// Current working directory (VFS path).
    pub cwd: PathBuf,
    /// Standard input for the tool (from pipeline).
    pub stdin: Option<String>,
}

impl ExecContext {
    /// Create a new execution context.
    pub fn new(vfs: Arc<VfsRouter>) -> Self {
        Self {
            vfs,
            scope: Scope::new(),
            cwd: PathBuf::from("/"),
            stdin: None,
        }
    }

    /// Create a context with a specific scope.
    pub fn with_scope(vfs: Arc<VfsRouter>, scope: Scope) -> Self {
        Self {
            vfs,
            scope,
            cwd: PathBuf::from("/"),
            stdin: None,
        }
    }

    /// Set stdin for this execution.
    pub fn set_stdin(&mut self, stdin: String) {
        self.stdin = Some(stdin);
    }

    /// Get stdin, consuming it.
    pub fn take_stdin(&mut self) -> Option<String> {
        self.stdin.take()
    }

    /// Resolve a path relative to cwd.
    pub fn resolve_path(&self, path: &str) -> PathBuf {
        if path.starts_with('/') {
            PathBuf::from(path)
        } else {
            self.cwd.join(path)
        }
    }

    /// Change the current working directory.
    pub fn set_cwd(&mut self, path: PathBuf) {
        self.cwd = path;
    }
}
