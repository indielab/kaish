//! MCP server for kaish.
//!
//! Exposes kaish as an MCP server for clients like Claude Code.
//!
//! # Server Usage
//!
//! ```ignore
//! use kaish_mcp::server::{KaishServerHandler, McpServerConfig};
//! use rmcp::transport::io::stdio;
//! use rmcp::service::ServiceExt;
//!
//! let config = McpServerConfig::load()?;
//! let handler = KaishServerHandler::new(config, vec![])?;
//! handler.serve(stdio()).await?;
//! ```

pub mod server;

pub use server::{KaishServerHandler, McpServerConfig};
