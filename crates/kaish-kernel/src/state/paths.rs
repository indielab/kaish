//! XDG Base Directory paths for kaish state.
//!
//! All runtime files follow XDG Base Directory Specification:
//!
//! | Purpose | XDG Variable | Default | kaish Path |
//! |---------|--------------|---------|------------|
//! | Sockets | `$XDG_RUNTIME_DIR` | `/run/user/$UID` | `$XDG_RUNTIME_DIR/kaish/*.sock` |
//! | State DB | `$XDG_DATA_HOME` | `~/.local/share` | `$XDG_DATA_HOME/kaish/kernels/*.db` |
//! | Blobs | `$XDG_DATA_HOME` | `~/.local/share` | `$XDG_DATA_HOME/kaish/blobs/` |
//! | Config | `$XDG_CONFIG_HOME` | `~/.config` | `$XDG_CONFIG_HOME/kaish/config.toml` |
//! | Cache | `$XDG_CACHE_HOME` | `~/.cache` | `$XDG_CACHE_HOME/kaish/` |

use std::path::PathBuf;

use directories::BaseDirs;

/// Get the runtime directory for sockets.
///
/// Uses `$XDG_RUNTIME_DIR/kaish` or falls back to `/tmp/kaish`.
pub fn runtime_dir() -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("kaish")
}

/// Get the data directory for persistent state.
///
/// Uses `$XDG_DATA_HOME/kaish` or falls back to `~/.local/share/kaish`.
pub fn data_dir() -> PathBuf {
    BaseDirs::new()
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| {
            dirs_fallback()
                .join(".local")
                .join("share")
        })
        .join("kaish")
}

/// Get the config directory.
///
/// Uses `$XDG_CONFIG_HOME/kaish` or falls back to `~/.config/kaish`.
pub fn config_dir() -> PathBuf {
    BaseDirs::new()
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| {
            dirs_fallback()
                .join(".config")
        })
        .join("kaish")
}

/// Get the cache directory.
///
/// Uses `$XDG_CACHE_HOME/kaish` or falls back to `~/.cache/kaish`.
pub fn cache_dir() -> PathBuf {
    BaseDirs::new()
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| {
            dirs_fallback()
                .join(".cache")
        })
        .join("kaish")
}

/// Get the kernels database directory.
///
/// Each kernel gets its own SQLite database at `data_dir()/kernels/{id}.db`.
pub fn kernels_dir() -> PathBuf {
    data_dir().join("kernels")
}

/// Get the blobs directory for large value storage.
pub fn blobs_dir() -> PathBuf {
    data_dir().join("blobs")
}

/// Fallback home directory when BaseDirs fails.
fn dirs_fallback() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_under_kaish() {
        assert!(runtime_dir().ends_with("kaish"));
        assert!(data_dir().ends_with("kaish"));
        assert!(config_dir().ends_with("kaish"));
        assert!(cache_dir().ends_with("kaish"));
    }

    #[test]
    fn kernels_dir_is_under_data() {
        let kernels = kernels_dir();
        let data = data_dir();
        assert!(kernels.starts_with(&data));
        assert!(kernels.ends_with("kernels"));
    }

    #[test]
    fn blobs_dir_is_under_data() {
        let blobs = blobs_dir();
        let data = data_dir();
        assert!(blobs.starts_with(&data));
        assert!(blobs.ends_with("blobs"));
    }
}
