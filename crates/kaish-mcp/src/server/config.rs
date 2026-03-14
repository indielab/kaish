//! Configuration for the kaish MCP server.
//!
//! Configuration is loaded from `~/.config/kaish/mcp-server.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Configuration for the kaish MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Server name (shown to MCP clients).
    #[serde(default = "default_name")]
    pub name: String,

    /// Server version. Not settable via config — always the binary's version.
    #[serde(skip, default = "default_version")]
    pub version: String,

    /// Resource mounts to expose via MCP.
    #[serde(default)]
    pub resource_mounts: Vec<ResourceMountConfig>,

    /// Default timeout for script execution in milliseconds.
    #[serde(default = "default_timeout")]
    pub default_timeout_ms: u64,
}

fn default_name() -> String {
    "kaish".to_string()
}

fn default_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn default_timeout() -> u64 {
    30_000
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: default_name(),
            version: default_version(),
            resource_mounts: Vec::new(),
            default_timeout_ms: default_timeout(),
        }
    }
}

impl McpServerConfig {
    /// Load configuration from the default path.
    ///
    /// If the config file doesn't exist, returns default configuration.
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;

        if !path.exists() {
            tracing::debug!("No config file at {}, using defaults", path.display());
            return Ok(Self::default());
        }

        Self::load_from(&path)
    }

    /// Load configuration from a specific path.
    pub fn load_from(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config from {}", path.display()))?;

        toml::from_str(&content)
            .with_context(|| format!("Failed to parse config from {}", path.display()))
    }

    /// Get the default config file path.
    pub fn config_path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("", "", "kaish")
            .context("Could not determine config directory")?;

        Ok(dirs.config_dir().join("mcp-server.toml"))
    }
}

/// Configuration for a resource mount.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceMountConfig {
    /// URI prefix for this mount (e.g., "kaish://vfs").
    pub uri_prefix: String,

    /// VFS path to expose (e.g., "/").
    pub vfs_path: String,
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = McpServerConfig::default();
        assert_eq!(config.name, "kaish");
        assert!(!config.version.is_empty());
        assert!(config.resource_mounts.is_empty());
        assert_eq!(config.default_timeout_ms, 30_000);
    }

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
name = "my-kaish"
version = "1.0.0"
default_timeout_ms = 60000

[[resource_mounts]]
uri_prefix = "kaish://vfs"
vfs_path = "/"
"#;

        let config: McpServerConfig = toml::from_str(toml).expect("parse failed");
        assert_eq!(config.name, "my-kaish");
        // version is #[serde(skip)] — always the binary version, not config-file value
        assert!(!config.version.is_empty());
        assert_eq!(config.default_timeout_ms, 60_000);
        assert_eq!(config.resource_mounts.len(), 1);
        assert_eq!(config.resource_mounts[0].uri_prefix, "kaish://vfs");
    }

    #[test]
    fn test_parse_minimal_config() {
        let toml = "";
        let config: McpServerConfig = toml::from_str(toml).expect("parse failed");
        assert_eq!(config.name, "kaish");
    }

}
