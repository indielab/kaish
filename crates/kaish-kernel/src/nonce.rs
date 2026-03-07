//! Confirmation nonce store for dangerous operations.
//!
//! Used by the latch system (`set -o latch`) to gate destructive commands
//! behind a nonce-based confirmation flow. Nonces are time-limited and
//! reusable within their TTL for idempotent retries.
//!
//! Nonces are path-scoped: a nonce issued for `rm fileA` cannot confirm
//! `rm fileB`. Validation checks both the command and that confirmed paths
//! are a subset of the authorized paths.

use std::collections::{BTreeSet, HashMap};
use std::hash::{BuildHasher, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::interpreter::ExecResult;

/// What a nonce authorizes: a command and a set of paths.
#[derive(Debug, Clone)]
pub struct NonceScope {
    /// Command name (e.g. "rm", "kaish-trash empty").
    command: String,
    /// Authorized paths. Empty means no path constraint (command-only ops).
    paths: BTreeSet<String>,
    /// Cached result for spill latch recovery (`--confirm=` path).
    cached_result: Option<ExecResult>,
}

impl NonceScope {
    /// The command this nonce authorizes (e.g. "rm").
    pub fn command(&self) -> &str {
        &self.command
    }

    /// The paths this nonce authorizes. Empty means command-only (no path constraint).
    pub fn paths(&self) -> &BTreeSet<String> {
        &self.paths
    }
}

/// A store for confirmation nonces with TTL-based expiration.
///
/// Nonces are 8-character hex strings that gate dangerous operations.
/// They remain valid until their TTL expires — not consumed on validation —
/// making operations idempotent: a retried `rm --confirm=abc123 bigdir/`
/// works if the nonce hasn't expired.
#[derive(Clone, Debug)]
pub struct NonceStore {
    inner: Arc<Mutex<NonceStoreInner>>,
    ttl: Duration,
}

#[derive(Debug)]
struct NonceStoreInner {
    /// Map from nonce string to (created_at, scope).
    nonces: HashMap<String, (Instant, NonceScope)>,
}

impl NonceStore {
    /// Create a new nonce store with the default TTL (60 seconds).
    pub fn new() -> Self {
        Self::with_ttl(Duration::from_secs(60))
    }

    /// Create a new nonce store with a custom TTL.
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(NonceStoreInner {
                nonces: HashMap::new(),
            })),
            ttl,
        }
    }

    /// Look up a nonce's scope without validating against a command/path.
    ///
    /// Returns the scope if the nonce exists and hasn't expired, or an error.
    /// Useful for embedders building custom confirmation UIs.
    pub fn lookup(&self, nonce: &str) -> Result<NonceScope, String> {
        let now = Instant::now();
        let ttl = self.ttl;

        #[allow(clippy::expect_used)]
        let inner = self.inner.lock().expect("nonce store poisoned");

        match inner.nonces.get(nonce) {
            Some((created, scope)) => {
                if now.duration_since(*created) >= ttl {
                    Err("nonce expired".to_string())
                } else {
                    Ok(scope.clone())
                }
            }
            None => Err("invalid nonce".to_string()),
        }
    }

    /// Issue a new nonce for the given command and paths.
    ///
    /// Returns an 8-character hex string. Opportunistically GCs expired nonces.
    pub fn issue(&self, command: &str, paths: &[&str]) -> String {
        let nonce = generate_nonce();
        let now = Instant::now();
        let ttl = self.ttl;

        let scope = NonceScope {
            command: command.to_string(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
            cached_result: None,
        };

        #[allow(clippy::expect_used)]
        let mut inner = self.inner.lock().expect("nonce store poisoned");

        // Opportunistic GC: remove expired nonces
        inner.nonces.retain(|_, (created, _)| now.duration_since(*created) < ttl);

        inner.nonces.insert(nonce.clone(), (now, scope));
        nonce
    }

    /// Issue a nonce that carries a cached `ExecResult` for spill latch recovery.
    ///
    /// When the agent runs `--confirm=<nonce>`, `get_cached_result` returns this
    /// result so the agent receives the truncated output with exit 0.
    pub fn issue_with_result(&self, result: ExecResult) -> String {
        let nonce = generate_nonce();
        let now = Instant::now();
        let ttl = self.ttl;

        let scope = NonceScope {
            command: String::new(),
            paths: BTreeSet::new(),
            cached_result: Some(result),
        };

        #[allow(clippy::expect_used)]
        let mut inner = self.inner.lock().expect("nonce store poisoned");

        inner.nonces.retain(|_, (created, _)| now.duration_since(*created) < ttl);
        inner.nonces.insert(nonce.clone(), (now, scope));
        nonce
    }

    /// Retrieve the cached result for a spill latch nonce.
    ///
    /// Returns `None` if the nonce is unknown, expired, or has no cached result.
    pub fn get_cached_result(&self, nonce: &str) -> Option<ExecResult> {
        let now = Instant::now();
        let ttl = self.ttl;

        #[allow(clippy::expect_used)]
        let inner = self.inner.lock().expect("nonce store poisoned");

        match inner.nonces.get(nonce) {
            Some((created, scope)) if now.duration_since(*created) < ttl => {
                scope.cached_result.clone()
            }
            _ => None,
        }
    }

    /// Validate a nonce against a command and paths.
    ///
    /// Checks that the nonce exists, hasn't expired, the command matches,
    /// and the confirmed paths are a subset of the authorized paths.
    ///
    /// Does NOT consume the nonce — it stays valid until TTL expires.
    pub fn validate(&self, nonce: &str, command: &str, paths: &[&str]) -> Result<(), String> {
        let now = Instant::now();
        let ttl = self.ttl;

        #[allow(clippy::expect_used)]
        let inner = self.inner.lock().expect("nonce store poisoned");

        match inner.nonces.get(nonce) {
            Some((created, scope)) => {
                if now.duration_since(*created) >= ttl {
                    return Err("nonce expired".to_string());
                }

                if scope.command != command {
                    return Err(format!(
                        "nonce scope mismatch: issued for command '{}', got '{}'",
                        scope.command, command
                    ));
                }

                // Every confirmed path must be in the authorized set.
                // Linear scan avoids BTreeSet allocation — slices are typically 0-1 elements.
                let unauthorized: Vec<_> = paths
                    .iter()
                    .filter(|p| !scope.paths.contains(**p))
                    .collect();

                if !unauthorized.is_empty() {
                    return Err(format!(
                        "nonce scope mismatch: authorized {:?}, got unauthorized {:?}",
                        scope.paths.iter().collect::<Vec<_>>(),
                        unauthorized
                    ));
                }

                Ok(())
            }
            None => Err("invalid nonce".to_string()),
        }
    }

    /// Get the TTL for nonces in this store.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }
}

impl Default for NonceStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Generate an 8-character hex nonce using RandomState + SystemTime.
fn generate_nonce() -> String {
    let hasher_state = std::collections::hash_map::RandomState::new();
    let mut hasher = hasher_state.build_hasher();

    // Mix in current time for uniqueness
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    hasher.write_u128(now.as_nanos());

    // Mix in a second RandomState for additional entropy
    let hasher_state2 = std::collections::hash_map::RandomState::new();
    let mut hasher2 = hasher_state2.build_hasher();
    hasher2.write_u64(0xdeadbeef);
    hasher.write_u64(hasher2.finish());

    format!("{:08x}", hasher.finish() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_and_validate() {
        let store = NonceStore::new();
        let nonce = store.issue("rm", &["/tmp/important"]);
        assert_eq!(nonce.len(), 8);
        assert!(nonce.chars().all(|c| c.is_ascii_hexdigit()));

        let result = store.validate(&nonce, "rm", &["/tmp/important"]);
        assert!(result.is_ok());
    }

    #[test]
    fn idempotent_reuse() {
        let store = NonceStore::new();
        let nonce = store.issue("rm", &["bigdir/"]);

        let first = store.validate(&nonce, "rm", &["bigdir/"]);
        let second = store.validate(&nonce, "rm", &["bigdir/"]);
        assert!(first.is_ok());
        assert!(second.is_ok());
    }

    #[test]
    fn expired_nonce_fails() {
        let store = NonceStore::with_ttl(Duration::from_millis(0));
        let nonce = store.issue("rm", &["ephemeral"]);

        // With 0ms TTL, nonce is immediately expired
        std::thread::sleep(Duration::from_millis(1));
        let result = store.validate(&nonce, "rm", &["ephemeral"]);
        assert_eq!(result, Err("nonce expired".to_string()));
    }

    #[test]
    fn invalid_nonce_fails() {
        let store = NonceStore::new();
        let result = store.validate("bogus123", "rm", &["anything"]);
        assert_eq!(result, Err("invalid nonce".to_string()));
    }

    #[test]
    fn nonces_are_unique() {
        let store = NonceStore::new();
        let a = store.issue("rm", &["first"]);
        let b = store.issue("rm", &["second"]);
        assert_ne!(a, b);
    }

    #[test]
    fn clone_shares_state() {
        let store = NonceStore::new();
        let cloned = store.clone();
        let nonce = store.issue("rm", &["/shared"]);

        let result = cloned.validate(&nonce, "rm", &["/shared"]);
        assert!(result.is_ok());
    }

    #[test]
    fn gc_cleans_expired() {
        let store = NonceStore::with_ttl(Duration::from_millis(10));
        let old_nonce = store.issue("rm", &["old"]);

        std::thread::sleep(Duration::from_millis(20));

        // This issue() triggers GC
        let _new = store.issue("rm", &["new"]);

        // Old nonce should be gone (GC'd)
        let result = store.validate(&old_nonce, "rm", &["old"]);
        assert!(result.is_err());
    }

    // ── Path-scoping tests ──

    #[test]
    fn path_mismatch_rejected() {
        let store = NonceStore::new();
        let nonce = store.issue("rm", &["fileA.txt"]);

        let result = store.validate(&nonce, "rm", &["fileB.txt"]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("nonce scope mismatch"));
    }

    #[test]
    fn subset_accepted() {
        let store = NonceStore::new();
        let nonce = store.issue("rm", &["a.txt", "b.txt", "c.txt"]);

        // Subset of authorized paths — should succeed
        let result = store.validate(&nonce, "rm", &["a.txt", "b.txt"]);
        assert!(result.is_ok());
    }

    #[test]
    fn superset_rejected() {
        let store = NonceStore::new();
        let nonce = store.issue("rm", &["a.txt", "b.txt"]);

        // Superset — c.txt not authorized
        let result = store.validate(&nonce, "rm", &["a.txt", "b.txt", "c.txt"]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unauthorized"));
    }

    #[test]
    fn command_mismatch_rejected() {
        let store = NonceStore::new();
        let nonce = store.issue("rm", &["file.txt"]);

        let result = store.validate(&nonce, "kaish-trash empty", &[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("command"));
    }

    #[test]
    fn empty_paths_command_only() {
        let store = NonceStore::new();
        let nonce = store.issue("kaish-trash empty", &[]);

        let result = store.validate(&nonce, "kaish-trash empty", &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn empty_paths_rejects_nonempty() {
        let store = NonceStore::new();
        let nonce = store.issue("kaish-trash empty", &[]);

        // Nonce was issued with no paths — can't use it to authorize a path
        let result = store.validate(&nonce, "kaish-trash empty", &["sneaky.txt"]);
        assert!(result.is_err());
    }

    // ── Spill latch cached result tests ──

    #[test]
    fn issue_with_result_and_retrieve() {
        let store = NonceStore::new();
        let result = crate::interpreter::ExecResult::success("truncated output");
        let nonce = store.issue_with_result(result.clone());

        assert_eq!(nonce.len(), 8);
        let retrieved = store.get_cached_result(&nonce);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().out, "truncated output");
    }

    #[test]
    fn get_cached_result_unknown_nonce() {
        let store = NonceStore::new();
        assert!(store.get_cached_result("bogus123").is_none());
    }

    #[test]
    fn cached_result_expired() {
        let store = NonceStore::with_ttl(Duration::from_millis(0));
        let result = crate::interpreter::ExecResult::success("data");
        let nonce = store.issue_with_result(result);

        std::thread::sleep(Duration::from_millis(1));
        assert!(store.get_cached_result(&nonce).is_none());
    }

    #[test]
    fn issue_nonce_has_no_cached_result() {
        let store = NonceStore::new();
        let nonce = store.issue("rm", &["/tmp/file"]);
        assert!(store.get_cached_result(&nonce).is_none());
    }
}
