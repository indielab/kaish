//! kaish-mcp: MCP server binary for kaish.
//!
//! # Transport: stdio only
//!
//! kaish-mcp communicates exclusively over stdio. A shell server that exposes
//! `$HOME` must not bind a network socket — any HTTP listener would make the
//! user's filesystem reachable from the network.
//!
//! The secure remote-access pattern is:
//!
//! 1. `kaish serve` runs a persistent kernel over a Unix socket
//!    (`$XDG_RUNTIME_DIR/kaish/<name>.sock`, mode 0600, Cap'n Proto RPC).
//! 2. MCP clients connect to `kaish-mcp` over stdio (the security boundary).
//! 3. Container runtimes work naturally: `docker exec`, `kubectl exec`, etc.
//!    all provide a stdio pipe that kaish-mcp plugs into directly.
//!
//! # Usage
//!
//! ```bash
//! kaish-mcp
//! ```

use anyhow::{Context, Result};
use opentelemetry::trace::TracerProvider;
use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;
use rmcp::service::ServiceExt;
use rmcp::transport::io::stdio;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use kaish_mcp::server::{KaishServerHandler, McpServerConfig};

#[tokio::main]
async fn main() -> Result<()> {
    // If OTEL_EXPORTER_OTLP_ENDPOINT is set, export spans via OTLP.
    // Otherwise, just use the fmt layer (no-op OTel).
    let provider = if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok() {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .build()
            .context("Failed to build OTLP exporter")?;
        let resource = Resource::builder()
            .with_attributes([
                KeyValue::new("service.name", "kaish-mcp"),
                KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
            ])
            .build();
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_resource(resource)
            .with_batch_exporter(exporter)
            .build();
        opentelemetry::global::set_tracer_provider(provider.clone());
        Some(provider)
    } else {
        None
    };

    let otel_layer = provider
        .as_ref()
        .map(|p| tracing_opentelemetry::layer().with_tracer(p.tracer("kaish-mcp")));

    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(otel_layer)
        .with(EnvFilter::from_default_env().add_directive("kaish_mcp=info".parse()?))
        .init();

    tracing::info!("Starting kaish MCP server");

    // Load configuration
    let config = McpServerConfig::load().context("Failed to load configuration")?;

    tracing::info!(
        "Server config: name={}, version={}, external_mcps={}",
        config.name,
        config.version,
        config.mcp_servers.len()
    );

    let handler = KaishServerHandler::new(config).context("Failed to create server handler")?;

    tracing::info!("Serving on stdio");

    let service = handler
        .serve(stdio())
        .await
        .context("Failed to start MCP service")?;

    service.waiting().await?;

    tracing::info!("Server shutdown complete");

    if let Some(provider) = provider {
        // Explicitly ignored: shutdown errors are non-fatal at process exit
        let _ = provider.shutdown();
    }

    Ok(())
}
