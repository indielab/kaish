//! MCP server handler implementation.
//!
//! Implements the rmcp::ServerHandler trait to expose kaish as an MCP server.
//! Manual impl (no `#[tool_handler]`) for full control over progress
//! notifications, prompt routing, and resource subscriptions.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;

use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    Annotated, CallToolRequestParams, CallToolResult, Content, GetPromptRequestParams,
    GetPromptResult, Implementation, ListPromptsResult, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProgressNotificationParam,
    ProtocolVersion, RawResource, RawResourceTemplate, ReadResourceRequestParams,
    ReadResourceResult, ResourceContents, ServerCapabilities, ServerInfo,
    SubscribeRequestParams, UnsubscribeRequestParams,
};
use rmcp::schemars::{self, JsonSchema};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ErrorData as McpError;
use rmcp::model::{PromptMessage, PromptMessageRole};
use rmcp::{prompt, prompt_router, tool, tool_router};
use serde::{Deserialize, Serialize};

use kaish_kernel::help::{get_help, HelpTopic};
use kaish_kernel::vfs::{LocalFs, MemoryFs, VfsRouter};

use super::config::McpServerConfig;
use super::execute::{self, ExecuteParams};
use super::resources::{self, parse_resource_uri, ResourceContent};
use super::subscriptions::SubscriptionTracker;

/// The kaish MCP server handler.
#[derive(Clone)]
pub struct KaishServerHandler {
    /// Server configuration.
    config: McpServerConfig,
    /// VFS router for resource access.
    vfs: Arc<VfsRouter>,
    /// Tool router.
    tool_router: ToolRouter<Self>,
    /// Prompt router.
    prompt_router: PromptRouter<Self>,
    /// Resource subscription tracker.
    subscriptions: Arc<SubscriptionTracker>,
}

impl KaishServerHandler {
    /// Create a new handler with the given configuration.
    pub fn new(config: McpServerConfig) -> anyhow::Result<Self> {
        // Create a VFS for resource access in sandboxed mode.
        // Paths appear native (e.g., /home/user/...) but access is restricted.
        let mut vfs = VfsRouter::new();

        // Mount memory at root for safety (catches paths outside sandbox)
        vfs.mount("/", MemoryFs::new());
        vfs.mount("/v", MemoryFs::new());

        // Per-handler /tmp — isolated but on-disk for interop with external commands.
        // Uses PID for uniqueness (one process = one session in stdio mode).
        let tmp_dir = std::env::temp_dir().join(format!("kaish-{}", std::process::id()));
        std::fs::create_dir_all(&tmp_dir)
            .context("Failed to create per-handler /tmp directory")?;
        vfs.mount("/tmp", LocalFs::new(tmp_dir));

        // Mount local filesystem at its real path for transparent access.
        // If HOME is not set, use a safe temp directory.
        let local_root = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                tracing::warn!("HOME not set, mounting temp directory");
                std::env::temp_dir()
            });
        let mount_point = local_root.to_string_lossy().to_string();
        vfs.mount(&mount_point, LocalFs::new(local_root));

        Ok(Self {
            config,
            vfs: Arc::new(vfs),
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
            subscriptions: SubscriptionTracker::new(),
        })
    }
}

/// Execute tool input schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExecuteInput {
    /// Kaish script or command to execute.
    #[schemars(description = "Kaish script or command to execute")]
    pub script: String,

    /// Initial working directory (default: $HOME).
    #[schemars(description = "Initial working directory (default: $HOME)")]
    pub cwd: Option<String>,

    /// Environment variables to set.
    #[schemars(description = "Environment variables to set")]
    pub env: Option<std::collections::HashMap<String, String>>,

    /// Timeout in milliseconds (default: 30000).
    #[schemars(description = "Timeout in milliseconds (default: 30000)")]
    pub timeout_ms: Option<u64>,
}

#[tool_router]
impl KaishServerHandler {
    /// Execute kaish shell scripts.
    ///
    /// Each call runs in a fresh, isolated environment. Supports restricted/modified
    /// Bourne syntax plus kaish extensions (scatter/gather, typed params, MCP tool calls).
    #[tool(description = "Execute kaish shell scripts. Fresh isolated environment per call.\n\nSupports: pipes, redirects, here-docs, if/for/while, functions, builtins (grep, jq, git, find, sed, awk, cat, ls, etc.), ${VAR:-default}, $((arithmetic)), scatter/gather parallelism.\n\nNOT supported: process substitution <(), backticks, eval, aliases, implicit word splitting.\n\nPaths: Native paths work within $HOME (e.g., /home/user/src/project). /scratch/ = ephemeral memory. Use 'help' tool for details.")]
    async fn execute(&self, input: Parameters<ExecuteInput>) -> Result<CallToolResult, McpError> {
        tracing::info!(
            script_len = input.0.script.len(),
            cwd = ?input.0.cwd,
            "mcp.execute"
        );

        let params = ExecuteParams {
            script: input.0.script,
            cwd: input.0.cwd,
            env: input.0.env,
            timeout_ms: input.0.timeout_ms,
        };

        let result =
            execute::execute(params, &self.config.mcp_servers, self.config.default_timeout_ms)
                .await
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Content blocks: plain text for human/LLM consumption
        let mut content = Vec::new();
        content.push(Content::text(&result.stdout));
        if !result.stderr.is_empty() {
            content.push(Content::text(format!("[stderr] {}", result.stderr)));
        }

        // Only include structured metadata when there's something beyond stdout
        // (errors, stderr, non-zero exit). Clean success → just the text.
        let structured_content = if !result.ok || !result.stderr.is_empty() {
            let structured = serde_json::to_value(&result)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            Some(structured)
        } else {
            None
        };

        Ok(CallToolResult {
            content,
            structured_content,
            is_error: Some(!result.ok),
            meta: None,
        })
    }
}

#[prompt_router(vis = "pub(crate)")]
impl KaishServerHandler {
    #[prompt(
        name = "kaish-overview",
        description = "What kaish is, topic list, quick examples"
    )]
    pub(crate) async fn prompt_overview(&self) -> Result<GetPromptResult, McpError> {
        let content = get_help(&HelpTopic::Overview, &[]);
        Ok(GetPromptResult {
            description: Some("kaish overview and quick reference".to_string()),
            messages: vec![PromptMessage::new_text(PromptMessageRole::User, content)],
        })
    }

    #[prompt(
        name = "kaish-syntax",
        description = "Variables, quoting, pipes, control flow reference"
    )]
    pub(crate) async fn prompt_syntax(&self) -> Result<GetPromptResult, McpError> {
        let content = get_help(&HelpTopic::Syntax, &[]);
        Ok(GetPromptResult {
            description: Some("kaish language syntax reference".to_string()),
            messages: vec![PromptMessage::new_text(PromptMessageRole::User, content)],
        })
    }

    #[prompt(
        name = "kaish-builtins",
        description = "List of all available builtins with descriptions"
    )]
    pub(crate) async fn prompt_builtins(
        &self,
        Parameters(params): Parameters<super::prompts::BuiltinsParams>,
    ) -> Result<GetPromptResult, McpError> {
        let (topic, description) = if let Some(tool_name) = params.tool {
            (
                HelpTopic::Tool(tool_name.clone()),
                format!("Help for builtin: {}", tool_name),
            )
        } else {
            (
                HelpTopic::Builtins,
                "All available kaish builtins".to_string(),
            )
        };

        // Create a temporary kernel to get tool schemas
        let config = kaish_kernel::KernelConfig::isolated().with_skip_validation(true);
        let kernel = kaish_kernel::Kernel::new(config)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let schemas = kernel.tool_schemas();
        let content = get_help(&topic, &schemas);

        Ok(GetPromptResult {
            description: Some(description),
            messages: vec![PromptMessage::new_text(PromptMessageRole::User, content)],
        })
    }

    #[prompt(
        name = "kaish-vfs",
        description = "Virtual filesystem mounts, paths, backends"
    )]
    pub(crate) async fn prompt_vfs(&self) -> Result<GetPromptResult, McpError> {
        let content = get_help(&HelpTopic::Vfs, &[]);
        Ok(GetPromptResult {
            description: Some("kaish VFS (virtual filesystem) reference".to_string()),
            messages: vec![PromptMessage::new_text(PromptMessageRole::User, content)],
        })
    }

    #[prompt(
        name = "kaish-scatter",
        description = "Parallel processing with scatter/gather (散/集)"
    )]
    pub(crate) async fn prompt_scatter(&self) -> Result<GetPromptResult, McpError> {
        let content = get_help(&HelpTopic::Scatter, &[]);
        Ok(GetPromptResult {
            description: Some("Scatter/gather parallel processing reference".to_string()),
            messages: vec![PromptMessage::new_text(PromptMessageRole::User, content)],
        })
    }

    #[prompt(
        name = "kaish-limits",
        description = "Known limitations and workarounds"
    )]
    pub(crate) async fn prompt_limits(&self) -> Result<GetPromptResult, McpError> {
        let content = get_help(&HelpTopic::Limits, &[]);
        Ok(GetPromptResult {
            description: Some("Known limitations and workarounds".to_string()),
            messages: vec![PromptMessage::new_text(PromptMessageRole::User, content)],
        })
    }
}

// Manual ServerHandler impl — replaces #[tool_handler] for full control
// over progress notifications, prompts, and subscriptions.
impl rmcp::ServerHandler for KaishServerHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::LATEST,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()

                .enable_prompts()
                .build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "kaish (会sh) — Predictable shell for MCP tool orchestration.\n\n\
                 Bourne-like syntax without the gotchas (no word splitting, no glob expansion, \
                 no backticks). Strict validation catches errors before execution. \
                 Builtins run in-process; external commands work via PATH fallback \
                 (just type `cargo build`, `git status`, etc.).\n\n\
                 Tools:\n\
                 • execute — Run shell scripts (pipes, redirects, builtins, loops, functions)\n\n\
                 Resources available via `kaish://vfs/{path}` URIs.\n\n\
                 Use 'help' tool for details."
                    .to_string(),
            ),
        }
    }

    // -- Tools (manual dispatch with progress notifications) --

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: self.tool_router.list_all(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // Extract progress token from request metadata
        use rmcp::model::RequestParamsMeta;
        let progress_token = request.progress_token();

        // Send "starting" progress notification
        if let Some(ref token) = progress_token {
            // Explicitly ignored: progress notifications are best-effort
            let _ = context
                .peer
                .notify_progress(ProgressNotificationParam {
                    progress_token: token.clone(),
                    progress: 0.0,
                    total: Some(1.0),
                    message: Some("Starting".to_string()),
                })
                .await;
        }

        // Dispatch to tool router
        let tcc = ToolCallContext::new(self, request, context);
        let result = self.tool_router.call(tcc).await;

        // Send "complete" progress notification (need to re-check token since context moved)
        if let Some(token) = progress_token {
            // Re-acquire peer from self — we can't use context.peer after move.
            // Progress token was captured before the move, so we just log completion.
            // Note: The peer was moved into ToolCallContext. For post-call progress,
            // we'd need to restructure. For now, start-only progress is the pattern
            // (the result itself signals completion).
            tracing::debug!(
                progress_token = ?token,
                "Tool call complete (progress token tracked)"
            );
        }

        result
    }

    fn get_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
        self.tool_router.get(name).cloned()
    }

    // -- Prompts --

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult {
            prompts: self.prompt_router.list_all(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        let prompt_context = rmcp::handler::server::prompt::PromptContext::new(
            self,
            request.name,
            request.arguments,
            context,
        );
        self.prompt_router.get_prompt(prompt_context).await
    }

    // -- Resources (unchanged) --

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult {
            resource_templates: vec![Annotated {
                raw: RawResourceTemplate {
                    uri_template: "kaish://vfs/{+path}".to_string(),
                    name: "VFS File".to_string(),
                    title: Some("Virtual Filesystem".to_string()),
                    description: Some(
                        "Access files and directories through kaish's VFS. \
                         Paths mirror the native filesystem under $HOME."
                            .to_string(),
                    ),
                    mime_type: None,
                    icons: None,
                },
                annotations: None,
            }],
            next_cursor: None,
            meta: None,
        })
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let resources = resources::list_resources(&self.vfs, std::path::Path::new("/"))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mcp_resources: Vec<Annotated<RawResource>> = resources
            .into_iter()
            .map(|r| Annotated {
                raw: RawResource {
                    uri: r.uri,
                    name: r.name,
                    title: None,
                    description: r.description,
                    mime_type: r.mime_type,
                    size: None,
                    icons: None,
                    meta: None,
                },
                annotations: None,
            })
            .collect();

        Ok(ListResourcesResult {
            resources: mcp_resources,
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let uri = request.uri.as_str();

        let path = parse_resource_uri(uri).ok_or_else(|| {
            McpError::invalid_request(format!("Invalid resource URI: {}", uri), None)
        })?;

        let content = resources::read_resource(&self.vfs, &path)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let contents = match content {
            ResourceContent::Text { text, mime_type } => {
                vec![ResourceContents::TextResourceContents {
                    uri: uri.to_string(),
                    mime_type: Some(mime_type),
                    text,
                    meta: None,
                }]
            }
            ResourceContent::Blob { data, mime_type } => {
                use base64::Engine;
                let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
                vec![ResourceContents::BlobResourceContents {
                    uri: uri.to_string(),
                    mime_type: Some(mime_type),
                    blob: encoded,
                    meta: None,
                }]
            }
        };

        Ok(ReadResourceResult { contents })
    }

    // -- Subscriptions --

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        tracing::info!(uri = %request.uri, "Resource subscription added");
        self.subscriptions.subscribe(request.uri).await;
        Ok(())
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        tracing::info!(uri = %request.uri, "Resource subscription removed");
        self.subscriptions.unsubscribe(&request.uri).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::RawContent;

    #[test]
    fn test_handler_creation() {
        let config = McpServerConfig::default();
        let handler = KaishServerHandler::new(config).expect("handler creation failed");
        assert_eq!(handler.config.name, "kaish");
    }

    #[test]
    fn test_get_info() {
        use rmcp::ServerHandler;

        let config = McpServerConfig::default();
        let handler = KaishServerHandler::new(config).expect("handler creation failed");
        let info = handler.get_info();
        assert!(info.instructions.is_some());
        let instructions = info.instructions.unwrap();
        assert!(instructions.contains("execute"));
    }

    #[test]
    fn test_get_info_capabilities() {
        use rmcp::ServerHandler;

        let config = McpServerConfig::default();
        let handler = KaishServerHandler::new(config).expect("handler creation failed");
        let info = handler.get_info();

        // Verify all expected capabilities are enabled
        assert!(info.capabilities.tools.is_some());
        assert!(info.capabilities.resources.is_some());
        assert!(info.capabilities.prompts.is_some());

        // Subscribe is NOT advertised (VFS doesn't emit change events yet)
        let resources = info.capabilities.resources.unwrap();
        assert!(!resources.subscribe.unwrap_or(false));
    }

    #[tokio::test]
    async fn test_execute_output_format() {
        let config = McpServerConfig::default();
        let handler = KaishServerHandler::new(config).expect("handler creation failed");

        let input = Parameters(ExecuteInput {
            script: "echo hello".to_string(),
            cwd: None,
            env: None,
            timeout_ms: None,
        });
        let result = handler.execute(input).await.expect("execute failed");

        // content[0] should be plain text (echo is simple text, not TOON-encoded)
        if let RawContent::Text(text) = &result.content[0].raw {
            assert_eq!(text.text.trim(), "hello");
            assert!(
                !text.text.contains(r#""code""#),
                "content should be plain text, not JSON metadata"
            );
        } else {
            panic!("Expected text content");
        }

        // Clean success → no structured_content (just text content blocks)
        assert!(
            result.structured_content.is_none(),
            "success should not have structured_content"
        );

        // is_error should be false for success
        assert_eq!(result.is_error, Some(false));
    }

    #[tokio::test]
    async fn test_execute_error_format() {
        let config = McpServerConfig::default();
        let handler = KaishServerHandler::new(config).expect("handler creation failed");

        let input = Parameters(ExecuteInput {
            script: "nonexistent_command_xyz".to_string(),
            cwd: None,
            env: None,
            timeout_ms: None,
        });
        let result = handler.execute(input).await.expect("execute failed");

        // is_error should be true
        assert_eq!(result.is_error, Some(true));

        // structured_content should have error details
        let structured = result
            .structured_content
            .expect("should have structured_content");
        assert_eq!(structured["ok"], false);
        assert_eq!(structured["code"], 127);
    }

    #[tokio::test]
    async fn test_execute_stderr_content() {
        let config = McpServerConfig::default();
        let handler = KaishServerHandler::new(config).expect("handler creation failed");

        // A command that fails produces stderr
        let input = Parameters(ExecuteInput {
            script: "nonexistent_command_xyz".to_string(),
            cwd: None,
            env: None,
            timeout_ms: None,
        });
        let result = handler.execute(input).await.expect("execute failed");

        // Should have a stderr content block with [stderr] prefix
        let stderr_block = result.content.iter().find(|c| {
            if let RawContent::Text(t) = &c.raw {
                t.text.starts_with("[stderr]")
            } else {
                false
            }
        });
        assert!(
            stderr_block.is_some(),
            "should have a [stderr] content block"
        );
    }
}
