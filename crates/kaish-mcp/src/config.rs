//! MCP configuration types.

use serde::{Deserialize, Serialize};

/// Configuration for connecting to an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    /// Human-readable name for this server.
    pub name: String,
    /// Transport configuration.
    pub transport: McpTransport,
}

/// Transport type for MCP connection.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpTransport {
    /// Stdio transport via child process.
    Stdio {
        /// Command to execute.
        command: String,
        /// Arguments to pass.
        #[serde(default)]
        args: Vec<String>,
        /// Environment variables.
        #[serde(default)]
        env: Vec<(String, String)>,
    },
}
