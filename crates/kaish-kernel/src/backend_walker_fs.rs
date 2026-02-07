//! `WalkerFs` adapter for `KernelBackend`.
//!
//! Bridges kaish-kernel's `KernelBackend` trait to kaish-glob's `WalkerFs`
//! trait so `FileWalker` and `IgnoreFilter` can work with any backend.

use async_trait::async_trait;
use std::path::Path;

use crate::backend::{EntryInfo, KernelBackend};
use kaish_glob::{WalkerDirEntry, WalkerError, WalkerFs};

/// Wraps a `&dyn KernelBackend` to implement `WalkerFs`.
pub struct BackendWalkerFs<'a>(pub &'a dyn KernelBackend);

impl WalkerDirEntry for EntryInfo {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_dir(&self) -> bool {
        self.is_dir
    }

    fn is_file(&self) -> bool {
        self.is_file
    }

    fn is_symlink(&self) -> bool {
        self.is_symlink
    }
}

#[async_trait]
impl WalkerFs for BackendWalkerFs<'_> {
    type DirEntry = EntryInfo;

    async fn list_dir(&self, path: &Path) -> Result<Vec<EntryInfo>, WalkerError> {
        self.0.list(path).await.map_err(|e| WalkerError::Io(e.to_string()))
    }

    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, WalkerError> {
        self.0.read(path, None).await.map_err(|e| WalkerError::Io(e.to_string()))
    }

    async fn is_dir(&self, path: &Path) -> bool {
        self.0.stat(path).await.is_ok_and(|info| info.is_dir)
    }

    async fn exists(&self, path: &Path) -> bool {
        self.0.exists(path).await
    }
}
