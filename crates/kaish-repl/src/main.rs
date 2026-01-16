//! kaish REPL entry point.
//!
//! Launch the interactive shell:
//! ```bash
//! cargo run -p kaish-repl
//! ```

use anyhow::Result;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

fn main() -> Result<()> {
    // Initialize tracing (respects RUST_LOG env var)
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    kaish_repl::run()
}
