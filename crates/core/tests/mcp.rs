//! MCP over the wire: a real rmcp client talking to our server over HTTP and over a pipe.

use anamnesis_core::config::Config;
use anamnesis_core::embed::{Embedder, HashEmbedder};
use anamnesis_core::mcp::{McpServer, McpStatus, Tools};
use anamnesis_core::search::Searcher;
use anamnesis_core::store::{ChunkRow, FileRecord, Store};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};

struct Vault {
    dir: tempfile::TempDir,
    searcher: Searcher,
}

fn vault() -> Vault {
    let dir = tempfile::tempdir().unwrap();
    let emb = Arc::new(HashEmbedder::new(32));
    let store = Arc::new(Store::open_in_memory("hash", 32).unwrap());
    let notes = [
        ("forge.md", "the anvil and the hammer shape hot iron"),
        ("garden.md", "tomatoes need sun and water"),
        ("big.md", "hammer"),
    ];
    for (name, text) in notes {
        let p = dir.path().join(name);
        std::fs::write(&p, text).unwrap();
        let n = if name == "big.md" { 3 } else { 1 };
        let chunks = (0..n)
            .map(|i| ChunkRow {
                chunk_index: i,
                heading: String::new(),
                context_path: String::new(),
                text: format!("{text} {i}"),
                embed_hash: format!("{name}{i}"),
                vector: emb.embed(&[text.to_string()]).unwrap().remove(0),
            })
            .collect();
        store
            .replace_file(&FileRecord {
                path: p.to_string_lossy().into(),
                mtime_ns: 1,
                content_hash: "h".into(),
                tags: "t".into(),
                chunks,
            })
            .unwrap();
    }
    let cfg = Config {
        watch_dirs: vec![dir.path().to_string_lossy().into()],
        search_results_limit: 2,
        ..Config::default()
    };
    Vault {
        searcher: Searcher {
            store,
            embedder: emb,
            cfg: Arc::new(RwLock::new(cfg)),
        },
        dir,
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn args(v: Value) -> serde_json::Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect()
}

async fn http_client(port: u16) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    ().serve(StreamableHttpClientTransport::from_uri(format!(
        "http://127.0.0.1:{port}/mcp"
    )))
    .await
    .unwrap()
}

#[tokio::test]
async fn http_server_lists_the_three_tools() {
    let v = vault();
    let port = free_port();
    let mut srv = McpServer::new(v.searcher.clone());
    srv.start(port).await.unwrap();
    assert_eq!((srv.status(), srv.port()), (McpStatus::Running, port));
    let c = http_client(port).await;
    let tools = c.list_all_tools().await.unwrap();
    let mut names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        vec!["list_indexed_files", "read_note", "search_vault"]
    );
    let search = tools.iter().find(|t| t.name == "search_vault").unwrap();
    let props = &search.input_schema["properties"];
    assert!(props.get("query").is_some() && props.get("limit").is_some());
    c.cancel().await.unwrap();
    srv.stop().await;
}

#[tokio::test]
async fn search_vault_returns_ts_shaped_hits_and_respects_limits() {
    let v = vault();
    let port = free_port();
    let mut srv = McpServer::new(v.searcher.clone());
    srv.start(port).await.unwrap();
    let c = http_client(port).await;

    let r = c
        .call_tool(
            CallToolRequestParams::new("search_vault")
                .with_arguments(args(json!({"query": "anvil hammer iron", "limit": 5}))),
        )
        .await
        .unwrap();
    assert_ne!(r.is_error, Some(true));
    let hits: Vec<Value> = serde_json::from_str(&text(&r)).unwrap();
    assert!(hits.len() <= 5 && !hits.is_empty());
    assert!(hits[0]["file_path"].as_str().unwrap().ends_with("forge.md"));
    for k in [
        "context_path",
        "heading",
        "chunk_index",
        "text",
        "tags",
        "importance_score",
        "match_sources",
        "score",
    ] {
        assert!(hits[0].get(k).is_some(), "missing {k}");
    }

    let r = c
        .call_tool(
            CallToolRequestParams::new("search_vault")
                .with_arguments(args(json!({"query": "hammer"}))),
        )
        .await
        .unwrap();
    let hits: Vec<Value> = serde_json::from_str(&text(&r)).unwrap();
    assert_eq!(hits.len(), 2, "default limit comes from searchResultsLimit");

    let r = c
        .call_tool(
            CallToolRequestParams::new("search_vault").with_arguments(args(json!({"query": "  "}))),
        )
        .await;
    assert!(
        r.is_err() || r.unwrap().is_error == Some(true),
        "blank query is rejected"
    );
    c.cancel().await.unwrap();
    srv.stop().await;
}

#[tokio::test]
async fn read_note_serves_vault_files_and_refuses_others() {
    let v = vault();
    let port = free_port();
    let mut srv = McpServer::new(v.searcher.clone());
    srv.start(port).await.unwrap();
    let c = http_client(port).await;

    let p = v.dir.path().join("garden.md");
    let r = c
        .call_tool(CallToolRequestParams::new("read_note").with_arguments(args(json!({"path": p}))))
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&text(&r)).unwrap();
    assert_eq!(body["content"], "tomatoes need sun and water");
    assert_eq!(body["word_count"], 5);
    assert!(body["path"].as_str().unwrap().ends_with("garden.md"));

    let outside = tempfile::NamedTempFile::new().unwrap();
    let r = c
        .call_tool(
            CallToolRequestParams::new("read_note")
                .with_arguments(args(json!({"path": outside.path()}))),
        )
        .await
        .unwrap();
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("outside"));

    let r = c
        .call_tool(
            CallToolRequestParams::new("read_note")
                .with_arguments(args(json!({"path": v.dir.path().join("missing.md")}))),
        )
        .await
        .unwrap();
    assert_eq!(r.is_error, Some(true));
    c.cancel().await.unwrap();
    srv.stop().await;
}

#[tokio::test]
async fn list_indexed_files_is_sorted_by_chunk_count() {
    let v = vault();
    let port = free_port();
    let mut srv = McpServer::new(v.searcher.clone());
    srv.start(port).await.unwrap();
    let c = http_client(port).await;
    let r = c
        .call_tool(CallToolRequestParams::new("list_indexed_files"))
        .await
        .unwrap();
    let files: Vec<Value> = serde_json::from_str(&text(&r)).unwrap();
    assert_eq!(files.len(), 3);
    assert!(files[0]["path"].as_str().unwrap().ends_with("big.md"));
    assert_eq!(files[0]["chunk_count"], 3);
    c.cancel().await.unwrap();
    srv.stop().await;
}

#[tokio::test]
async fn busy_port_is_a_readable_error_and_restart_works() {
    let v = vault();
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = blocker.local_addr().unwrap().port();
    let mut srv = McpServer::new(v.searcher.clone());
    let err = srv.start(port).await.unwrap_err().to_string();
    assert!(
        err.contains(&format!("Port {port} is already in use")),
        "{err}"
    );
    assert_eq!(srv.status(), McpStatus::Error);
    assert!(srv.error().is_some());
    drop(blocker);
    srv.start(port).await.unwrap();
    assert_eq!(srv.status(), McpStatus::Running);
    assert!(srv.error().is_none());
    srv.stop().await;
    srv.stop().await; // idempotent
    assert_eq!(srv.status(), McpStatus::Stopped);
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err(),
        "port released"
    );
    srv.start(port).await.unwrap();
    srv.stop().await;
}

#[tokio::test]
async fn http_rejects_foreign_host_headers_and_sends_no_wildcard_cors() {
    let v = vault();
    let port = free_port();
    let mut srv = McpServer::new(v.searcher.clone());
    srv.start(port).await.unwrap();
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/mcp");
    let evil = client
        .post(&url)
        .header("Host", "evil.example")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(
        evil.status().is_client_error(),
        "DNS-rebinding host must be refused, got {}",
        evil.status()
    );
    let pre = client
        .request(reqwest::Method::OPTIONS, &url)
        .header("Origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert_ne!(
        pre.headers()
            .get("access-control-allow-origin")
            .map(|h| h.to_str().unwrap()),
        Some("*")
    );
    let other = client
        .get(format!("http://127.0.0.1:{port}/other"))
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), 404);
    srv.stop().await;
}

#[tokio::test]
async fn same_tools_work_over_a_stdio_style_pipe() {
    let v = vault();
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let tools = Tools::new(v.searcher.clone());
    tokio::spawn(async move {
        let running = tools.serve(server_io).await.unwrap();
        let _ = running.waiting().await;
    });
    let c = ().serve(client_io).await.unwrap();
    let r = c
        .call_tool(CallToolRequestParams::new("list_indexed_files"))
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<Value>>(&text(&r)).unwrap().len(),
        3
    );
    c.cancel().await.unwrap();
}
