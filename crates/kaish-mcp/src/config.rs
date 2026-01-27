//! MCP configuration types.

/// Configuration for connecting to an MCP server.
#[derive(Debug, Clone)]
pub struct McpConfig {
    /// Human-readable name for this server.
    pub name: String,
    /// Transport configuration.
    pub transport: McpTransport,
}

/// Transport type for MCP connection.
#[derive(Debug, Clone)]
pub enum McpTransport {
    /// Stdio transport via child process.
    Stdio {
        /// Command to execute.
        command: String,
        /// Arguments to pass.
        args: Vec<String>,
        /// Environment variables.
        env: Vec<(String, String)>,
    },
}
