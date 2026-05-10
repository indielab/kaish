//! Kernel-level execution options.
//!
//! `ExecuteOptions` is the input to a single kernel `execute` call. It collects
//! the per-call knobs (variables, timeout, cancellation) so embedders don't need
//! to manage half a dozen execute-method overloads.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::value::Value;

/// Per-call options for `Kernel::execute_with_options`.
///
/// Construct with `ExecuteOptions::new()` and the chainable `with_*` builders,
/// or via `Default`.
///
/// # Cancellation vs. timeout — embedder note
///
/// If a `cancel_token` is supplied, it is **raced** against the kernel's
/// internal token. The kernel does NOT cancel the embedder's token on its
/// own timeouts — it cancels its internal token and returns exit code 124.
/// So `your_token.is_cancelled()` after the call returns reflects only
/// whether *you* (or someone sharing your token) cancelled, not whether the
/// kernel timed out. Distinguish via the returned `ExecResult.code`:
/// `124` = kernel timeout, `130` = cancellation (Ctrl-C / `Kernel::cancel`).
#[derive(Default, Clone)]
pub struct ExecuteOptions {
    /// Variables exported into this call's environment (per-call overlay).
    pub vars: HashMap<String, Value>,
    /// Per-call timeout. Overrides `KernelConfig::request_timeout`.
    ///
    /// `None` means no timeout (or whatever the kernel-config default is).
    /// `Some(Duration::ZERO)` returns exit 124 immediately without spawning
    /// anything — useful for tests and dry-run paths.
    /// Any other `Some(d)` lets the kernel run for at most `d` before cancelling
    /// (which kills external children with the configured grace) and returning 124.
    pub timeout: Option<Duration>,
    /// Optional externally-owned cancellation token, *raced* against the kernel's
    /// internal token. Either firing cancels the request and kills any running
    /// external children. The kernel does not store this token in its own state —
    /// it's a per-call read-only input, so embedders are free to drop or reuse
    /// the original token after the call returns. CancellationToken is internally
    /// `Arc`-shared, so `clone()` it into the builder if you want to keep your
    /// original handle.
    pub cancel_token: Option<CancellationToken>,
    /// Per-call working directory override.
    ///
    /// When `Some(path)`, the kernel runs this call as if `cd path` happened
    /// first, then restores the prior cwd on return. Useful for embedders that
    /// run scripts in workspace contexts (notebook cells, per-tool dirs)
    /// without polluting the long-lived kernel's cwd.
    pub cwd: Option<PathBuf>,
}

impl ExecuteOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the entire vars overlay with the given map.
    pub fn with_vars(mut self, vars: HashMap<String, Value>) -> Self {
        self.vars = vars;
        self
    }

    /// Add a single variable to the overlay (extending; last write wins).
    pub fn with_var(mut self, name: impl Into<String>, value: Value) -> Self {
        self.vars.insert(name.into(), value);
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn with_cancel_token(mut self, token: CancellationToken) -> Self {
        self.cancel_token = Some(token);
        self
    }

    /// Run this call as if `cd path` had happened first; the prior cwd is
    /// restored on return.
    pub fn with_cwd(mut self, cwd: PathBuf) -> Self {
        self.cwd = Some(cwd);
        self
    }
}
