//! Engine end to end on a temp vault with the deterministic HashEmbedder.

use anamnesis_core::config::Config;
use anamnesis_core::embed::{Embedder, HashEmbedder};
use anamnesis_core::engine::Engine;
use anamnesis_core::indexer::IndexStatus;
use anamnesis_core::mcp::McpStatus;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const IDLE: Duration = Duration::from_secs(20);

struct T {
    root: tempfile::TempDir,
    vault: PathBuf,
    cfg_path: PathBuf,
}

impl T {
    fn new() -> T {
        T::with(|_| {})
    }
    fn with(tweak: impl FnOnce(&mut Config)) -> T {
        let root = tempfile::tempdir().unwrap();
        let vault = root.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(vault.join("forge.md"), "# Forge\nanvil hammer iron").unwrap();
        std::fs::write(vault.join("garden.md"), "tomatoes sun water").unwrap();
        let cfg_path = root.path().join("cfg").join("config.json");
        let mut c = Config::load(&cfg_path);
        c.watch_dirs = vec![vault.to_string_lossy().into()];
        c.indexing_debounce_ms = 500;
        c.mcp_port = free_port();
        tweak(&mut c);
        c.save(&cfg_path).unwrap();
        T {
            root,
            vault,
            cfg_path,
        }
    }
    fn open(&self) -> Arc<Engine> {
        self.open_with(Arc::new(HashEmbedder::new(32)))
    }
    fn open_with(&self, e: Arc<dyn Embedder>) -> Arc<Engine> {
        let eng = Engine::open(&self.cfg_path, e).unwrap();
        assert!(eng.wait_idle(IDLE));
        eng
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn until(mut f: impl FnMut() -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < IDLE {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn indexed(e: &Engine) -> Vec<String> {
    let mut v: Vec<String> = e
        .search("anvil hammer iron tomatoes sun water new note", Some(100))
        .unwrap()
        .into_iter()
        .map(|h| h.hit.file_path)
        .collect();
    v.sort();
    v.dedup();
    v
}

fn name(p: &str) -> &str {
    Path::new(p).file_name().unwrap().to_str().unwrap()
}

#[test]
fn opening_indexes_the_vault_and_search_works() {
    let t = T::new();
    let e = t.open();
    assert_eq!(e.status().chunk_count, 2);
    let hits = e.search("anvil", None).unwrap();
    assert_eq!(name(&hits[0].hit.file_path), "forge.md");
    assert!(hits[0].match_sources.contains(&"bm25"));
}

#[test]
fn status_payload_has_the_ts_keys() {
    let t = T::new();
    let e = t.open();
    let v = serde_json::to_value(e.status()).unwrap();
    for k in [
        "status",
        "indexStatus",
        "mcpStatus",
        "mcpPort",
        "chunkCount",
        "model",
        "embeddingProvider",
        "dimension",
        "watchDirs",
    ] {
        assert!(v.get(k).is_some(), "missing {k}");
    }
    assert_eq!(v["status"], "running");
    assert_eq!(v["indexStatus"]["state"], "idle");
    assert_eq!(v["embeddingProvider"], "local");
    assert_eq!(v["dimension"], 32);
}

#[test]
fn reopening_is_incremental_and_a_new_dimension_rebuilds() {
    let t = T::new();
    drop(t.open());
    let e = t.open();
    assert_eq!(e.status().chunk_count, 2, "reopen keeps the index");
    drop(e);
    let e = t.open_with(Arc::new(HashEmbedder::new(16)));
    assert_eq!(
        (e.status().dimension, e.status().chunk_count),
        (16, 2),
        "dim change wiped and re-indexed"
    );
}

#[test]
fn watcher_indexes_new_and_deleted_files() {
    let t = T::new();
    let e = t.open();
    let p = t.vault.join("new.md");
    std::fs::write(&p, "new note").unwrap();
    assert!(
        until(|| indexed(&e).iter().any(|f| name(f) == "new.md")),
        "new file indexed by the watcher"
    );
    std::fs::remove_file(&p).unwrap();
    assert!(
        until(|| !indexed(&e).iter().any(|f| name(f) == "new.md")),
        "deleted file removed"
    );
}

#[test]
fn queued_status_is_reported_then_flush_indexes_immediately() {
    let t = T::with(|c| c.indexing_debounce_ms = 60_000);
    let e = t.open();
    let seen = Arc::new(Mutex::new(vec![]));
    let s2 = seen.clone();
    e.on_status(move |p| s2.lock().unwrap().push(p.index_status));
    std::fs::write(t.vault.join("new.md"), "new note").unwrap();
    // Count is not pinned: FSEvents (macOS) may also replay the fixture's own writes from just
    // before the stream started, so the queue can hold more than the one new file.
    assert!(until(|| seen.lock().unwrap().iter().any(|s| matches!(
        s,
        IndexStatus::Queued {
            count: 1..,
            delay_ms: 60_000,
            ..
        }
    ))));
    e.flush();
    assert!(until(|| indexed(&e).iter().any(|f| name(f) == "new.md")));
}

#[test]
fn auto_index_off_means_no_watcher() {
    let t = T::with(|c| c.auto_index_on_change = false);
    let e = t.open();
    std::fs::write(t.vault.join("new.md"), "new note").unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert!(!indexed(&e).iter().any(|f| name(f) == "new.md"));
    e.reindex();
    assert!(e.wait_idle(IDLE));
    assert!(
        indexed(&e).iter().any(|f| name(f) == "new.md"),
        "manual re-index still picks it up"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn saving_config_adds_and_removes_watch_dirs() {
    let t = T::new();
    let e = t.open();
    let second = t.root.path().join("second");
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(second.join("extra.md"), "hammer extra").unwrap();
    let dirs = vec![
        t.vault.to_string_lossy().to_string(),
        second.to_string_lossy().to_string(),
    ];
    assert!(!e.save_config(&json!({ "watchDirs": dirs })).await.unwrap());
    assert!(e.wait_idle(IDLE));
    assert!(indexed(&e).iter().any(|f| name(f) == "extra.md"));
    assert_eq!(Config::load(&t.cfg_path).watch_dirs.len(), 2, "persisted");
    e.save_config(&json!({ "watchDirs": [t.vault] }))
        .await
        .unwrap();
    assert!(e.wait_idle(IDLE));
    assert!(
        !indexed(&e).iter().any(|f| name(f) == "extra.md"),
        "removed dir purged"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn saving_excludes_purges_and_model_change_requests_restart() {
    let t = T::new();
    let e = t.open();
    e.save_config(&json!({ "excludePatterns": ["garden.md"] }))
        .await
        .unwrap();
    assert!(e.wait_idle(IDLE));
    assert_eq!(
        indexed(&e)
            .iter()
            .map(|f| name(f).to_string())
            .collect::<Vec<_>>(),
        vec!["forge.md"]
    );
    assert!(e
        .save_config(&json!({ "localModelName": "BAAI/bge-small-en-v1.5" }))
        .await
        .unwrap());
    assert!(e.save_config(&json!({ "chunkSize": 128 })).await.is_ok());
    assert!(e.save_config(&json!({ "chunkSize": "big" })).await.is_err());
}

#[test]
fn dirs_report_chunks_and_pause_state() {
    let t = T::new();
    let e = t.open();
    let d = t.vault.to_string_lossy().to_string();
    let dirs = e.dirs().unwrap();
    assert_eq!(
        (dirs[0].path.as_str(), dirs[0].chunk_count, dirs[0].paused),
        (d.as_str(), 2, false)
    );
    e.pause_dir(&d);
    assert!(e.dirs().unwrap()[0].paused);
    std::fs::write(t.vault.join("new.md"), "new note").unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !indexed(&e).iter().any(|f| name(f) == "new.md"),
        "paused dir ignores changes"
    );
    e.resume_dir(&d);
    e.reindex_dir(&d);
    assert!(e.wait_idle(IDLE));
    assert!(indexed(&e).iter().any(|f| name(f) == "new.md"));
}

#[test]
fn vectors_feed_the_graph() {
    let t = T::new();
    let e = t.open();
    let nodes = e.vectors(2000).unwrap();
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0].vector.len(), 32);
    assert_eq!(e.vectors(1).unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_lifecycle_follows_config_and_commands() {
    let t = T::new();
    let e = t.open();
    e.start_mcp().await.unwrap();
    let s = e.status();
    assert_eq!(s.mcp_status, McpStatus::Running);
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", s.mcp_port))
        .await
        .is_ok());
    e.stop_mcp().await;
    assert_eq!(e.status().mcp_status, McpStatus::Stopped);
    let port = free_port();
    e.save_config(&json!({ "mcpPort": port })).await.unwrap();
    e.start_mcp().await.unwrap();
    assert_eq!(e.status().mcp_port, port);
    e.save_config(&json!({ "mcpEnabled": false }))
        .await
        .unwrap();
    assert_eq!(
        e.status().mcp_status,
        McpStatus::Stopped,
        "disabling stops the server"
    );
    e.shutdown().await;
}
