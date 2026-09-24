//! MCP server: `search_vault`, `read_note`, `list_indexed_files` over streamable HTTP
//! (`127.0.0.1:<port>/mcp`, same URL and tool shapes as the TS server) or stdio.

use crate::search::Searcher;
use anyhow::Result;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData, ServerHandler, ServiceExt};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum McpStatus {
    Stopped,
    Running,
    Error,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchArgs {
    #[schemars(description = "Natural language search query")]
    pub query: String,
    #[schemars(description = "Maximum results (default from settings, max 100)")]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadArgs {
    #[schemars(description = "Absolute path to the file")]
    pub path: String,
}

/// The rmcp tool handler. Cheap to clone; one per HTTP session.
#[derive(Clone)]
pub struct Tools {
    searcher: Searcher,
}

fn json_result(v: &impl Serialize) -> Result<CallToolResult, ErrorData> {
    let text = serde_json::to_string_pretty(v)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

fn tool_error(msg: impl Into<String>) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::error(vec![ContentBlock::text(msg.into())]))
}

/// SQLite and ORT calls block; keep them off the async executor.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T, ErrorData> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
}

#[tool_router]
impl Tools {
    pub fn new(searcher: Searcher) -> Tools {
        Tools { searcher }
    }

    #[tool(
        description = "Hybrid semantic + keyword search over indexed files. Combines vector similarity with BM25 via Reciprocal Rank Fusion."
    )]
    async fn search_vault(
        &self,
        Parameters(a): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if a.query.trim().is_empty() {
            return tool_error("query must not be empty");
        }
        let s = self.searcher.clone();
        let hits = blocking(move || s.search(&a.query, a.limit.map(|l| l as usize))).await?;
        json_result(&hits)
    }

    #[tool(description = "Read the full content of an indexed file by its absolute path.")]
    async fn read_note(
        &self,
        Parameters(a): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let dirs = self
            .searcher
            .cfg
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .watch_dirs
            .clone();
        let path = match resolve_note(&dirs, &a.path) {
            Ok(p) => p,
            Err(e) => return tool_error(e),
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return tool_error(format!("Could not read {}: {e}", a.path)),
        };
        let word_count = content.split_whitespace().count();
        json_result(
            &serde_json::json!({ "path": a.path, "word_count": word_count, "content": content }),
        )
    }

    #[tool(description = "List all currently indexed files with their chunk counts.")]
    async fn list_indexed_files(&self) -> Result<CallToolResult, ErrorData> {
        let store = self.searcher.store.clone();
        let files = blocking(move || store.file_chunk_counts()).await?;
        let files: Vec<_> = files.into_iter().map(|(path, chunk_count)| serde_json::json!({ "path": path, "chunk_count": chunk_count })).collect();
        json_result(&files)
    }
}

#[tool_handler]
impl ServerHandler for Tools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("Anamnesis", env!("CARGO_PKG_VERSION")))
            .with_instructions("Semantic + keyword search over the user's local notes. Use search_vault first, then read_note for full files.")
    }
}

/// `read_note` only serves files inside a watch dir. Symlinks and `..` are resolved first.
pub fn resolve_note(watch_dirs: &[String], path: &str) -> Result<PathBuf, String> {
    let real = Path::new(path)
        .canonicalize()
        .map_err(|_| format!("File not found: {path}"))?;
    if !real.is_file() {
        return Err(format!("Not a file: {path}"));
    }
    let inside = watch_dirs
        .iter()
        .filter_map(|d| Path::new(d).canonicalize().ok())
        .any(|d| real.starts_with(d));
    if !inside {
        return Err(format!(
            "Refusing to read {path}: outside the watched folders"
        ));
    }
    Ok(real)
}

/// Lifecycle of the HTTP server (start/stop from the tray, UI or config changes).
pub struct McpServer {
    searcher: Searcher,
    running: Option<(CancellationToken, tokio::task::JoinHandle<()>)>,
    port: u16,
    error: Option<String>,
}

impl McpServer {
    pub fn new(searcher: Searcher) -> McpServer {
        McpServer {
            searcher,
            running: None,
            port: 0,
            error: None,
        }
    }

    /// Binds `127.0.0.1:port`. Restarts if already running. A busy port is an error with a
    /// readable message and leaves the status at `Error`.
    pub async fn start(&mut self, port: u16) -> Result<()> {
        self.stop().await;
        self.port = port;
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => l,
            Err(e) => {
                let msg = if e.kind() == std::io::ErrorKind::AddrInUse {
                    format!("Port {port} is already in use")
                } else {
                    e.to_string()
                };
                self.error = Some(msg.clone());
                anyhow::bail!(msg);
            }
        };
        let ct = CancellationToken::new();
        // Defaults keep rmcp's loopback-only Host allowlist (DNS rebinding guard) and send no CORS headers.
        let config = StreamableHttpServerConfig::default().with_cancellation_token(ct.clone());
        let searcher = self.searcher.clone();
        let service: StreamableHttpService<Tools, LocalSessionManager> = StreamableHttpService::new(
            move || Ok(Tools::new(searcher.clone())),
            Default::default(),
            config,
        );
        let router = axum::Router::new().nest_service("/mcp", service);
        let shutdown = ct.clone();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { shutdown.cancelled_owned().await })
                .await;
        });
        self.error = None;
        self.running = Some((ct, task));
        tracing::info!("MCP server listening on http://127.0.0.1:{port}/mcp");
        Ok(())
    }

    pub async fn stop(&mut self) {
        if let Some((ct, task)) = self.running.take() {
            ct.cancel();
            let _ = task.await;
            tracing::info!("MCP server stopped");
        }
    }

    pub fn status(&self) -> McpStatus {
        match (&self.running, &self.error) {
            (Some(_), _) => McpStatus::Running,
            (None, Some(_)) => McpStatus::Error,
            (None, None) => McpStatus::Stopped,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }
}

/// Serves MCP over stdin/stdout until the client disconnects (headless `--stdio`).
pub async fn serve_stdio(searcher: Searcher) -> Result<()> {
    let running = Tools::new(searcher).serve(rmcp::transport::stdio()).await?;
    running.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_note_allows_files_inside_watch_dirs() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.md");
        std::fs::write(&p, "x").unwrap();
        let dirs = vec![d.path().to_string_lossy().into_owned()];
        assert_eq!(
            resolve_note(&dirs, &p.to_string_lossy()).unwrap(),
            p.canonicalize().unwrap()
        );
    }

    #[test]
    fn resolve_note_rejects_outside_traversal_missing_and_directories() {
        let vault = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let secret = other.path().join("secret.txt");
        std::fs::write(&secret, "s").unwrap();
        std::fs::create_dir(vault.path().join("sub")).unwrap();
        let dirs = vec![vault.path().to_string_lossy().into_owned()];
        let err = resolve_note(&dirs, &secret.to_string_lossy()).unwrap_err();
        assert!(err.contains("outside"), "{err}");
        let sneaky = vault
            .path()
            .join("sub")
            .join("..")
            .join("..")
            .join(other.path().file_name().unwrap())
            .join("secret.txt");
        assert!(resolve_note(&dirs, &sneaky.to_string_lossy())
            .unwrap_err()
            .contains("outside"));
        assert!(
            resolve_note(&dirs, &vault.path().join("nope.md").to_string_lossy())
                .unwrap_err()
                .contains("not found")
        );
        assert!(resolve_note(&dirs, &vault.path().join("sub").to_string_lossy()).is_err());
        assert!(
            resolve_note(&[], &secret.to_string_lossy()).is_err(),
            "no watch dirs = nothing readable"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_note_rejects_symlinks_escaping_the_vault() {
        let vault = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let secret = other.path().join("secret.md");
        std::fs::write(&secret, "s").unwrap();
        let link = vault.path().join("link.md");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        let dirs = vec![vault.path().to_string_lossy().into_owned()];
        assert!(resolve_note(&dirs, &link.to_string_lossy())
            .unwrap_err()
            .contains("outside"));
    }

    #[test]
    fn status_serializes_lowercase() {
        assert_eq!(serde_json::to_value(McpStatus::Running).unwrap(), "running");
    }
}
