//! Kernel-level execution options.
//!
//! `ExecuteOptions` is the input to a single kernel `execute` call. It collects
//! the per-call knobs (variables, timeout, cancellation) so embedders don't need
//! to manage half a dozen execute-method overloads.

use std::collections::HashMap;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::value::Value;

/// Per-call options for `Kernel::execute_with_options`.
///
/// Construct with `ExecuteOptions::new()` and the chainable `with_*` builders,
/// or via `Default`.
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
    /// the original token after the call returns.
    pub cancel_token: Option<CancellationToken>,
}

impl ExecuteOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_vars(mut self, vars: HashMap<String, Value>) -> Self {
        self.vars = vars;
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
}
