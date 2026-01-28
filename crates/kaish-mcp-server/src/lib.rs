//! kaish-mcp-server: Expose kaish as an MCP server.
//!
//! This crate provides an MCP server that exposes kaish shell execution
//! capabilities to MCP clients like Claude Code.
//!
//! # Features
//!
//! - **execute tool**: Run kaish scripts in a fresh, isolated kernel
//! - **VFS resources**: Access filesystem via MCP resource protocol
//! - **MCP chaining**: Configure external MCP servers for tool integration
//!
//! # Example
//!
//! ```ignore
//! use kaish_mcp_server::{KaishServerHandler, McpServerConfig};
//! use rmcp::transport::io::stdio;
//! use rmcp::service::ServiceExt;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let config = McpServerConfig::load()?;
//!     let handler = KaishServerHandler::new(config)?;
//!
//!     let transport = stdio();
//!     handler.serve(transport).await?;
//!     Ok(())
//! }
//! ```

pub mod config;
pub mod execute;
pub mod handler;
pub mod resources;

pub use config::McpServerConfig;
pub use handler::KaishServerHandler;
