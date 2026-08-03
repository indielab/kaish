//! Redemption-time precondition verification (spec §B.4, ledger PR 6): the
//! registry of [`StateResolver`]s, the kernel's `path` resolver, and the
//! [`ConditionReport`] a redemption carries into the ledger's critical
//! section.
//!
//! The split matters. Observing a resource is I/O, so it happens **outside**
//! the ledger lock (spec §B.1); the ledger then commits the reservation
//! against what was observed, and records it on the `Redeemed` entry. So the
//! ledger detects an authorization that went stale — it does not make the
//! mutation atomic. Closing that window is the resource's own job: a git
//! compare-and-swap ref update, or the backend's conditional write.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use kaish_types::approval::{Condition, Observation, ResourceRef, StateClaim};
use kaish_types::backend::ReadRange;
use sha2::Digest;

use crate::backend::KernelBackend;

pub use kaish_tool_api::{ResolverError, StateResolver};

/// The resource kind the kernel owns. A registered resolver claiming it is
/// refused at kernel construction, the way the `fs.` operation namespace is
/// (spec §A.6) — a plugin that could shadow `path` could decide whether a
/// file counts as changed.
pub const PATH_KIND: &str = "path";

/// The digest algorithm the kernel's `path` resolver records, and the only
/// one it compares against. Recorded on every `StateClaim::Digest` it
/// produces so a reader never has to guess.
pub const PATH_DIGEST_ALG: &str = "sha256";

/// How much of a file the `path` resolver hashes per backend read. Matches
/// the streaming readers' window, so a backend that overrides `read_range`
/// digests a large file without holding it in memory.
const DIGEST_WINDOW: u64 = 256 * 1024;

/// What the resolvers saw for one grant's conditions, evaluated outside the
/// ledger lock and carried into it (spec §B.1).
///
/// [`Self::Unobservable`] is a distinct variant rather than a missing
/// observation because the two mean different things: a missing observation
/// says "nobody looked", and the ledger would have to guess. This says "we
/// looked and could not tell", and the ledger refuses on it — the same
/// refusal a changed resource gets, because a precondition nobody could
/// check has not been met.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ConditionReport {
    /// Every condition that claimed a prior state was observed.
    Observed(Vec<Observation>),
    /// One resource could not be observed at all.
    Unobservable {
        /// Which resource.
        resource: ResourceRef,
        /// What went wrong, as the resolver stated it.
        detail: String,
    },
}

impl ConditionReport {
    /// Nothing to report — the grant declared no condition that claims a
    /// prior state, so there was nothing to observe.
    pub fn none() -> Self {
        Self::Observed(Vec::new())
    }

    /// What a resolver saw for each condition that claimed a prior state.
    pub fn observed(observations: Vec<Observation>) -> Self {
        Self::Observed(observations)
    }
}

/// The resolvers a kernel can consult, keyed by resource kind.
///
/// Holds only the kinds an embedder or plugin registered:
/// [`PATH_KIND`] is served by a [`PathResolver`] built per execution from
/// that execution's own backend and cwd, so a request posted inside an
/// overlay is re-observed through the same overlay.
#[derive(Default, Clone)]
pub struct StateResolvers {
    by_kind: HashMap<String, Arc<dyn StateResolver>>,
}

impl StateResolvers {
    /// Build a registry from the resolvers an embedder registered, in
    /// registration order.
    ///
    /// # Errors
    ///
    /// A resolver claiming [`PATH_KIND`], or a second resolver for a kind
    /// already registered. Both are loud at construction rather than a
    /// last-one-wins overwrite — which resolver decided whether a resource
    /// changed must never depend on registration order.
    pub fn from_registrations(
        resolvers: Vec<Arc<dyn StateResolver>>,
    ) -> Result<Self, StateResolverConflict> {
        let mut by_kind: HashMap<String, Arc<dyn StateResolver>> = HashMap::new();
        for resolver in resolvers {
            let kind = resolver.kind().to_string();
            if kind == PATH_KIND {
                return Err(StateResolverConflict::Reserved);
            }
            if by_kind.contains_key(&kind) {
                return Err(StateResolverConflict::Duplicate(kind));
            }
            by_kind.insert(kind, resolver);
        }
        Ok(Self { by_kind })
    }

    /// The resolver registered for `kind`, if any. `path` is never here —
    /// see the type's own doc.
    pub fn get(&self, kind: &str) -> Option<&Arc<dyn StateResolver>> {
        self.by_kind.get(kind)
    }

    /// Every registered kind, sorted, for a diagnostic that has to say what
    /// *is* available.
    pub fn kinds(&self) -> Vec<&str> {
        let mut kinds: Vec<&str> = self.by_kind.keys().map(String::as_str).collect();
        kinds.sort_unstable();
        kinds
    }
}

/// Why a set of registered resolvers was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateResolverConflict {
    /// A registered resolver claimed [`PATH_KIND`].
    Reserved,
    /// Two registered resolvers claimed the same kind.
    Duplicate(String),
}

impl std::fmt::Display for StateResolverConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reserved => write!(
                f,
                "the '{PATH_KIND}' resource kind belongs to the kernel; register a resolver under \
                 your own kind (e.g. 'git.ref') instead"
            ),
            Self::Duplicate(kind) => write!(
                f,
                "two state resolvers claim the resource kind '{kind}'; exactly one may"
            ),
        }
    }
}

impl std::error::Error for StateResolverConflict {}

/// The kernel's resolver for `path` resources: the target's content digest,
/// read through the backend so a real, overlay, or in-memory file is
/// observed the same way.
///
/// A path that does not exist observes [`StateClaim::Absent`], which is a
/// successful observation — "the file was deleted since the grant" is a
/// state, not a failure. Everything else that stops the read from finishing
/// is a [`ResolverError`] and refuses.
pub struct PathResolver {
    backend: Arc<dyn KernelBackend>,
    cwd: PathBuf,
}

impl PathResolver {
    /// Resolve `path` resources against `backend`, taking `cwd` as the
    /// directory a relative resource id is named from — the same cwd the
    /// gate site resolved it in.
    pub fn new(backend: Arc<dyn KernelBackend>, cwd: PathBuf) -> Self {
        Self { backend, cwd }
    }
}

#[async_trait]
impl StateResolver for PathResolver {
    fn kind(&self) -> &str {
        PATH_KIND
    }

    async fn observe(&self, id: &str) -> Result<StateClaim, ResolverError> {
        if id.is_empty() {
            return Err(ResolverError::InvalidId("empty path".to_string()));
        }
        let resolved = if id.starts_with('/') {
            PathBuf::from(id)
        } else {
            self.cwd.join(id)
        };
        digest_path(&*self.backend, &resolved).await
    }
}

/// The content digest of `path` as a [`StateClaim`], or [`StateClaim::Absent`]
/// when it does not exist.
///
/// The one digest implementation: a gate site that has to declare "the file
/// is at this digest now" and the resolver that re-checks it at redemption
/// both come through here, so the claim and the check can never drift apart.
pub(crate) async fn digest_path(
    backend: &dyn KernelBackend,
    path: &Path,
) -> Result<StateClaim, ResolverError> {
    if !backend.exists(path).await {
        return Ok(StateClaim::Absent);
    }
    let entry = backend
        .stat(path)
        .await
        .map_err(|e| ResolverError::Io(format!("{}: {e}", path.display())))?;
    if entry.is_dir() {
        return Err(ResolverError::Unsupported(format!(
            "{}: a directory has no content digest",
            path.display()
        )));
    }

    let mut hasher = sha2::Sha256::new();
    let mut offset = 0u64;
    loop {
        let chunk = backend
            .read(path, Some(ReadRange::bytes(offset, DIGEST_WINDOW)))
            .await
            .map_err(|e| ResolverError::Io(format!("{}: {e}", path.display())))?;
        if chunk.is_empty() {
            break;
        }
        hasher.update(&chunk);
        offset += chunk.len() as u64;
        if (chunk.len() as u64) < DIGEST_WINDOW {
            break;
        }
    }
    Ok(StateClaim::Digest {
        alg: PATH_DIGEST_ALG.to_string(),
        hex: hex_lower(hasher.finalize().as_slice()),
    })
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // Writing to a String is infallible; the Result exists only because
        // `fmt::Write` is generic over sinks that can fail.
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Which of `conditions` actually claim a prior state.
///
/// A condition whose `expected_from` is [`StateClaim::Unspecified`] claims
/// nothing, so there is nothing to observe and nothing to check — a grant
/// whose conditions are all `Unspecified` is unconditioned, and its
/// `Redeemed` entry records an empty observation set to say so (spec §A.3).
/// This is not `Unspecified`-as-wildcard: it never compares equal to a
/// concrete claim, it is simply never compared.
pub(crate) fn conditions_to_observe(conditions: &[Condition]) -> Vec<&Condition> {
    conditions
        .iter()
        .filter(|c| c.expected_from != StateClaim::Unspecified)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubResolver(&'static str);

    #[async_trait]
    impl StateResolver for StubResolver {
        fn kind(&self) -> &str {
            self.0
        }
        async fn observe(&self, _id: &str) -> Result<StateClaim, ResolverError> {
            Ok(StateClaim::Unspecified)
        }
    }

    #[test]
    fn path_is_reserved_for_the_kernel() {
        let err = StateResolvers::from_registrations(vec![Arc::new(StubResolver("path"))])
            .expect_err("registering 'path' must be refused");
        assert_eq!(err, StateResolverConflict::Reserved);
    }

    #[test]
    fn a_kind_may_be_registered_once() {
        let err = StateResolvers::from_registrations(vec![
            Arc::new(StubResolver("git.ref")),
            Arc::new(StubResolver("git.ref")),
        ])
        .expect_err("a duplicate kind must be refused");
        assert_eq!(err, StateResolverConflict::Duplicate("git.ref".to_string()));
    }

    #[test]
    fn unspecified_conditions_are_not_observed() {
        let conditions = vec![
            Condition {
                resource: ResourceRef {
                    kind: "path".to_string(),
                    id: "/a".to_string(),
                },
                expected_from: StateClaim::Unspecified,
            },
            Condition {
                resource: ResourceRef {
                    kind: "path".to_string(),
                    id: "/b".to_string(),
                },
                expected_from: StateClaim::Absent,
            },
        ];
        let to_observe = conditions_to_observe(&conditions);
        assert_eq!(to_observe.len(), 1);
        assert_eq!(to_observe[0].resource.id, "/b");
    }

    #[test]
    fn hex_is_lowercase_and_zero_padded() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xff]), "000fff");
    }
}
