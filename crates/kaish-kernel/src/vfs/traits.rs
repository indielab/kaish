//! Core VFS traits and types.

use async_trait::async_trait;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Kind of directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirEntryKind {
    File,
    Directory,
    Symlink,
}

/// A directory entry — the unified file metadata type.
///
/// Used everywhere: VFS `list()`, `stat()`, `lstat()`, and `KernelBackend` methods.
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// Name of the entry (not full path).
    pub name: String,
    /// Kind of entry.
    pub kind: DirEntryKind,
    /// Size in bytes (0 for directories).
    pub size: u64,
    /// Last modification time, if available.
    pub modified: Option<SystemTime>,
    /// Unix permissions (e.g., 0o644), if available.
    pub permissions: Option<u32>,
    /// For symlinks, the target path.
    pub symlink_target: Option<PathBuf>,
}

impl DirEntry {
    /// Create a new directory entry.
    pub fn directory(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: DirEntryKind::Directory,
            size: 0,
            modified: None,
            permissions: None,
            symlink_target: None,
        }
    }

    /// Create a new file entry.
    pub fn file(name: impl Into<String>, size: u64) -> Self {
        Self {
            name: name.into(),
            kind: DirEntryKind::File,
            size,
            modified: None,
            permissions: None,
            symlink_target: None,
        }
    }

    /// Create a new symlink entry.
    pub fn symlink(name: impl Into<String>, target: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            kind: DirEntryKind::Symlink,
            size: 0,
            modified: None,
            permissions: None,
            symlink_target: Some(target.into()),
        }
    }
}

/// Abstract filesystem interface.
///
/// All operations use paths relative to the filesystem root.
/// For example, if a `LocalFs` is rooted at `/home/amy/project`,
/// then `read("src/main.rs")` reads `/home/amy/project/src/main.rs`.
#[async_trait]
pub trait Filesystem: Send + Sync {
    /// Read the entire contents of a file.
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// Write data to a file, creating it if it doesn't exist.
    ///
    /// Returns `Err` if the filesystem is read-only.
    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()>;

    /// List entries in a directory.
    async fn list(&self, path: &Path) -> io::Result<Vec<DirEntry>>;

    /// Get metadata for a file or directory.
    async fn stat(&self, path: &Path) -> io::Result<DirEntry>;

    /// Create a directory (and parent directories if needed).
    ///
    /// Returns `Err` if the filesystem is read-only.
    async fn mkdir(&self, path: &Path) -> io::Result<()>;

    /// Remove a file or empty directory.
    ///
    /// Returns `Err` if the filesystem is read-only.
    async fn remove(&self, path: &Path) -> io::Result<()>;

    /// Returns true if this filesystem is read-only.
    fn read_only(&self) -> bool;

    /// Check if a path exists.
    async fn exists(&self, path: &Path) -> bool {
        self.stat(path).await.is_ok()
    }

    /// Rename (move) a file or directory.
    ///
    /// This is an atomic operation when source and destination are on the same
    /// filesystem. The default implementation falls back to copy+delete, which
    /// is not atomic.
    ///
    /// Returns `Err` if the filesystem is read-only.
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        // Default implementation: copy then delete (not atomic)
        let entry = self.stat(from).await?;
        if entry.kind == DirEntryKind::Directory {
            // For directories, we'd need recursive copy - just error for now
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "rename directories not supported by this filesystem",
            ));
        }
        let data = self.read(from).await?;
        self.write(to, &data).await?;
        self.remove(from).await?;
        Ok(())
    }

    /// Get the real filesystem path for a VFS path.
    ///
    /// Returns `Some(path)` for backends backed by the real filesystem (like LocalFs),
    /// or `None` for virtual backends (like MemoryFs).
    ///
    /// This is needed for tools like `git` that must use real paths with external libraries.
    fn real_path(&self, path: &Path) -> Option<PathBuf> {
        let _ = path;
        None
    }

    /// Read the target of a symbolic link without following it.
    ///
    /// Returns the path the symlink points to. Use `stat` to follow symlinks.
    async fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "symlinks not supported by this filesystem",
        ))
    }

    /// Create a symbolic link.
    ///
    /// Creates a symlink at `link` pointing to `target`. The target path
    /// is stored as-is (may be relative or absolute).
    async fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        let _ = (target, link);
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "symlinks not supported by this filesystem",
        ))
    }

    /// Get metadata for a path without following symlinks.
    ///
    /// Unlike `stat`, this returns metadata about the symlink itself,
    /// not the target it points to.
    async fn lstat(&self, path: &Path) -> io::Result<DirEntry> {
        // Default: same as stat (for backends that don't support symlinks)
        self.stat(path).await
    }
}
