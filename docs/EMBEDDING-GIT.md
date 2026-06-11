# Embedding kaish: Git Integration

How an embedder wires git into kaish — the `git` builtin against custom
mount layouts, and direct `GitVfs` access for repository plumbing. For the
core embedding guide (kernel construction, configuration, custom tools),
see [EMBEDDING.md](EMBEDDING.md).

Everything here requires the **`git` capability feature** on `kaish-kernel`
(not part of the default feature set):

```toml
kaish-kernel = { version = "0.8", features = ["git"] }
```

The implementation lives in the `kaish-tools-git` crate; `kaish-kernel`
re-exports `GitVfs`, `FileStatus`, `StatusSummary`, `LogEntry`, and
`WorktreeInfo` at its root when the feature is on.

## Custom Backend for Git Operations

The key to getting git operations "for free" is implementing
`resolve_real_path()` on your `KernelBackend`. It tells kaish how to map
VFS paths to the real filesystem paths where git repositories live.

### Example: kaijutsu-style Worktrees

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;
use kaish_kernel::{
    Kernel, KernelConfig, KernelBackend, LocalBackend,
    xdg_data_home,
};
use kaish_kernel::vfs::{LocalFs, VfsRouter};

/// Custom backend that maps VFS paths to kaijutsu worktrees
struct KaijutsuBackend {
    /// Delegate to LocalBackend for file operations
    inner: LocalBackend,
    /// Root of worktrees directory
    worktrees_root: PathBuf,
}

impl KaijutsuBackend {
    fn new() -> Self {
        let worktrees_root = xdg_data_home()
            .join("kaijutsu")
            .join("worktrees");

        // LocalBackend routes file I/O through a VfsRouter the embedder owns.
        let mut vfs = VfsRouter::new();
        vfs.mount("/mnt/repos", LocalFs::new(&worktrees_root));

        Self {
            inner: LocalBackend::new(Arc::new(vfs)),
            worktrees_root,
        }
    }
}

impl KernelBackend for KaijutsuBackend {
    // ... delegate most methods to self.inner ...

    /// Map VFS paths to real worktree paths
    fn resolve_real_path(&self, path: &Path) -> Option<PathBuf> {
        // /mnt/repos/kaish/src/main.rs → ~/.local/share/kaijutsu/worktrees/kaish/src/main.rs
        if let Ok(rest) = path.strip_prefix("/mnt/repos") {
            return Some(self.worktrees_root.join(rest));
        }

        // Other mounts...
        None
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let backend = Arc::new(KaijutsuBackend::new());
    let config = KernelConfig::named("kaijutsu");

    let kernel = Kernel::with_backend(backend, config, |_| {}, |_| {})?;

    // Git operations now work on worktrees!
    kernel.execute("cd /mnt/repos/kaish && git status").await?;

    Ok(())
}
```

### How Git Operations Work

When a script runs `git status`:

1. The `git` builtin receives the current working directory (e.g., `/mnt/repos/kaish`)
2. It calls `backend.resolve_real_path(&cwd)`
3. Your backend returns the real path (e.g., `~/.local/share/kaijutsu/worktrees/kaish`)
4. kaish opens a `GitVfs` at that real path
5. Git operations work directly on the worktree

The `git` builtin operates on the **real filesystem** — it won't work on
memory-only mounts where `resolve_real_path` returns `None`.

## Direct GitVfs Access

For lower-level git operations, use `GitVfs` directly:

```rust
use kaish_kernel::{GitVfs, FileStatus, LogEntry, WorktreeInfo};
use std::path::Path;

fn inspect_repo() -> anyhow::Result<()> {
    let repo = GitVfs::open("/path/to/worktree")?;

    // Get current branch
    if let Some(branch) = repo.current_branch()? {
        println!("On branch: {}", branch);
    }

    // Check status
    let status = repo.status()?;
    for file in &status {
        println!("{} {}", file.status_char(), file.path);
    }

    // Stage and commit
    repo.add(&["src/*.rs"])?;
    repo.commit("Update source files", None)?;

    // View log
    for entry in repo.log(5)? {
        println!("{} {}", entry.short_id, entry.message.lines().next().unwrap_or(""));
    }

    Ok(())
}

fn manage_worktrees() -> anyhow::Result<()> {
    let repo = GitVfs::open("/path/to/main/repo")?;

    // List all worktrees
    for wt in repo.worktrees()? {
        println!("{}: {} ({:?})",
            wt.name.as_deref().unwrap_or("main"),
            wt.path.display(),
            wt.head
        );
    }

    // Create a new worktree for a feature branch
    let wt_info = repo.worktree_add(
        "feature-work",
        Path::new("/path/to/feature-worktree"),
        Some("feature-branch"),  // existing branch, or None for new branch
    )?;
    println!("Created worktree at {}", wt_info.path.display());

    // Lock a worktree to prevent accidental pruning
    repo.worktree_lock("feature-work", Some("work in progress"))?;

    // Later, unlock and remove
    repo.worktree_unlock("feature-work")?;
    repo.worktree_remove("feature-work", false)?;  // force=false

    // Clean up stale worktree entries
    let pruned = repo.worktree_prune()?;
    println!("Pruned {} stale worktree(s)", pruned);

    Ok(())
}
```

`GitVfs` also provides `clone`, `init`, `diff`, `branches`,
`create_branch`, `checkout`, `status_summary`, `add_path`, and
`reset_path` — see `kaish-tools-git` for the full surface.

## Best Practices

1. **Use `resolve_real_path()`** — this is the key abstraction. Map your
   VFS paths to real paths where git repos live.

2. **Direct `GitVfs` for complex operations** — for operations beyond what
   the `git` builtin provides, use `GitVfs` directly.

3. **Handle worktrees vs bare repos** — the `git` builtin works on
   worktrees (real files). If you use bare repos internally, map VFS paths
   to worktree paths, not bare repo paths.
