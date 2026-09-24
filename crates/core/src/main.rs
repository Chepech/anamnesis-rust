//! Headless Anamnesis: indexer + watcher + MCP, no UI.
//!
//!   anamnesis-core [--config PATH] [--stdio]
//!
//! `--stdio` serves MCP on stdin/stdout (for clients that launch servers as a subprocess);
//! otherwise MCP is served over HTTP on `127.0.0.1:<mcpPort>/mcp` until Ctrl+C.

use anamnesis_core::config::{default_config_path, Config};
use anamnesis_core::engine::Engine;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs go to stderr: stdout belongs to the MCP protocol in --stdio mode.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("anamnesis-core [--config PATH] [--stdio]");
        return Ok(());
    }
    let config_path = args
        .iter()
        .position(|a| a == "--config")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(default_config_path);
    let stdio = args.iter().any(|a| a == "--stdio");

    let cfg = Config::load(&config_path);
    if cfg.watch_dirs.is_empty() {
        tracing::warn!("no watchDirs configured in {}", config_path.display());
    }
    tracing::info!("loading embedding model {}", cfg.local_model_name);
    let embedder = tokio::task::spawn_blocking(move || Engine::load_embedder(&cfg)).await??;
    let engine = Engine::open(&config_path, embedder)?;

    if stdio {
        anamnesis_core::mcp::serve_stdio(engine.searcher()).await?;
    } else {
        if engine.config().mcp_enabled {
            engine.start_mcp().await?;
        }
        tracing::info!("ready; Ctrl+C to stop");
        tokio::signal::ctrl_c().await?;
    }
    engine.shutdown().await;
    Ok(())
}
