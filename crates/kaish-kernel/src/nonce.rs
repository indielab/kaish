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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kaish_types::clock::Instant;

/// What a nonce authorizes: a command and a set of paths.
#[derive(Debug, Clone)]
pub struct NonceScope {
    /// Command name (e.g. "rm", "kaish-trash empty").
    command: String,
    /// Authorized paths. Empty means no path constraint (command-only ops).
    paths: BTreeSet<String>,
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
    /// Returns a 32-character hex string (128 bits from the OS CSPRNG). Opportunistically
    /// GCs expired nonces.
    ///
    /// # Errors
    ///
    /// Propagates `getrandom::Error` if the OS entropy source fails. There is
    /// deliberately no fallback: a guessable nonce would let an attacker
    /// forge a `--confirm` for a destructive op, so callers must fail loudly
    /// rather than silently degrade (see `generate_nonce`).
    pub fn issue(&self, command: &str, paths: &[&str]) -> Result<String, getrandom::Error> {
        let nonce = generate_nonce()?;
        let now = Instant::now();
        let ttl = self.ttl;

        let scope = NonceScope {
            command: command.to_string(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
        };

        #[allow(clippy::expect_used)]
        let mut inner = self.inner.lock().expect("nonce store poisoned");

        // Opportunistic GC: remove expired nonces
        inner.nonces.retain(|_, (created, _)| now.duration_since(*created) < ttl);

        inner.nonces.insert(nonce.clone(), (now, scope));
        Ok(nonce)
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
                // Short-circuit on first unauthorized path — slices are typically 0-1 elements.
                if let Some(unauthorized) = paths.iter().find(|p| !scope.paths.contains(**p)) {
                    return Err(format!(
                        "nonce scope mismatch: unauthorized path '{}' (authorized: {:?})",
                        unauthorized,
                        scope.paths.iter().collect::<Vec<_>>()
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

/// Generate a 32-character hex nonce (128 bits) from the OS CSPRNG.
///
/// Pulls raw entropy via `getrandom` (already a kernel dependency for
/// `mktemp`'s temp-name generation) rather than deriving the nonce from a
/// hasher seeded with wall-clock time — a confirmation token gates
/// destructive operations, so it must not be guessable or collidable by an
/// adversary who can observe or influence the clock.
///
/// There is deliberately **no fallback** to a weaker generator: if the OS
/// cannot supply entropy, issuing a nonce fails loudly (propagated to the
/// caller) rather than silently emitting a guessable confirmation token.
///
/// Rate-limiting repeated wrong `--confirm` guesses is explicitly out of
/// scope here — it needs a request/attempt-identity model that doesn't exist
/// yet (a rejected guess doesn't currently identify which issued nonce it
/// was aimed at). Deferred, not implemented, in this change.
fn generate_nonce() -> Result<String, getrandom::Error> {
    let mut entropy = [0u8; 16];
    getrandom::fill(&mut entropy)?;
    Ok(entropy.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_and_validate() {
        let store = NonceStore::new();
        let nonce = store.issue("rm", &["/tmp/important"]).expect("entropy");
        assert_eq!(nonce.len(), 32);
        assert!(nonce.chars().all(|c| c.is_ascii_hexdigit()));

        let result = store.validate(&nonce, "rm", &["/tmp/important"]);
        assert!(result.is_ok());
    }

    // ── CSPRNG hardening (nonce widened from a 32-bit hash-derived value to
    // 128 bits of `getrandom` entropy) ──

    #[test]
    fn nonce_is_32_lowercase_hex_chars() {
        let store = NonceStore::new();
        let nonce = store.issue("rm", &["/tmp/important"]).expect("entropy");
        assert_eq!(nonce.len(), 32, "128 bits rendered as hex is 32 chars: {nonce}");
        assert!(
            nonce.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "nonce must be lowercase hex: {nonce}"
        );
    }

    #[test]
    fn no_collisions_across_a_large_batch() {
        let store = NonceStore::new();
        let mut seen = std::collections::HashSet::with_capacity(100_000);
        for i in 0..100_000 {
            let nonce = store
                .issue("rm", &["/tmp/batch"])
                .unwrap_or_else(|e| panic!("entropy failure at iteration {i}: {e}"));
            assert!(seen.insert(nonce), "collision detected within 100_000 issued nonces");
        }
    }

    #[test]
    fn back_to_back_nonces_differ_under_identical_conditions() {
        // Guards against a time/hash-derived generator sneaking back in: if the
        // generator were seeded from wall-clock time alone, two calls issued
        // in immediate succession (same command, same paths, same instant to
        // clock resolution) could collide. A CSPRNG draw should not.
        let store = NonceStore::new();
        let a = store.issue("rm", &["same/path"]).expect("entropy");
        let b = store.issue("rm", &["same/path"]).expect("entropy");
        assert_ne!(a, b, "back-to-back nonces must differ even under identical scope/timing");
    }

    #[test]
    fn idempotent_reuse() {
        let store = NonceStore::new();
        let nonce = store.issue("rm", &["bigdir/"]).expect("entropy");

        let first = store.validate(&nonce, "rm", &["bigdir/"]);
        let second = store.validate(&nonce, "rm", &["bigdir/"]);
        assert!(first.is_ok());
        assert!(second.is_ok());
    }

    #[test]
    fn expired_nonce_fails() {
        let store = NonceStore::with_ttl(Duration::from_millis(0));
        let nonce = store.issue("rm", &["ephemeral"]).expect("entropy");

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
        let a = store.issue("rm", &["first"]).expect("entropy");
        let b = store.issue("rm", &["second"]).expect("entropy");
        assert_ne!(a, b);
    }

    #[test]
    fn clone_shares_state() {
        let store = NonceStore::new();
        let cloned = store.clone();
        let nonce = store.issue("rm", &["/shared"]).expect("entropy");

        let result = cloned.validate(&nonce, "rm", &["/shared"]);
        assert!(result.is_ok());
    }

    #[test]
    fn gc_cleans_expired() {
        let store = NonceStore::with_ttl(Duration::from_millis(10));
        let old_nonce = store.issue("rm", &["old"]).expect("entropy");

        std::thread::sleep(Duration::from_millis(20));

        // This issue() triggers GC
        let _new = store.issue("rm", &["new"]).expect("entropy");

        // Old nonce should be gone (GC'd)
        let result = store.validate(&old_nonce, "rm", &["old"]);
        assert!(result.is_err());
    }

    // ── Path-scoping tests ──

    #[test]
    fn path_mismatch_rejected() {
        let store = NonceStore::new();
        let nonce = store.issue("rm", &["fileA.txt"]).expect("entropy");

        let result = store.validate(&nonce, "rm", &["fileB.txt"]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("nonce scope mismatch"));
    }

    #[test]
    fn subset_accepted() {
        let store = NonceStore::new();
        let nonce = store.issue("rm", &["a.txt", "b.txt", "c.txt"]).expect("entropy");

        // Subset of authorized paths — should succeed
        let result = store.validate(&nonce, "rm", &["a.txt", "b.txt"]);
        assert!(result.is_ok());
    }

    #[test]
    fn superset_rejected() {
        let store = NonceStore::new();
        let nonce = store.issue("rm", &["a.txt", "b.txt"]).expect("entropy");

        // Superset — c.txt not authorized
        let result = store.validate(&nonce, "rm", &["a.txt", "b.txt", "c.txt"]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unauthorized"));
    }

    #[test]
    fn command_mismatch_rejected() {
        let store = NonceStore::new();
        let nonce = store.issue("rm", &["file.txt"]).expect("entropy");

        let result = store.validate(&nonce, "kaish-trash empty", &[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("command"));
    }

    #[test]
    fn empty_paths_command_only() {
        let store = NonceStore::new();
        let nonce = store.issue("kaish-trash empty", &[]).expect("entropy");

        let result = store.validate(&nonce, "kaish-trash empty", &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn empty_paths_rejects_nonempty() {
        let store = NonceStore::new();
        let nonce = store.issue("kaish-trash empty", &[]).expect("entropy");

        // Nonce was issued with no paths — can't use it to authorize a path
        let result = store.validate(&nonce, "kaish-trash empty", &["sneaky.txt"]);
        assert!(result.is_err());
    }

}
