//! The running app: config + store + embedder + indexer worker + watcher + MCP server.
//! Both the Tauri shell and the headless binary drive everything through this type
//! (it replaces `daemon.ts`, its management HTTP API and the Electron `CoreManager`).

use crate::config::Config;
use crate::embed::{Embedder, FastEmbedder};
use crate::filter::Filter;
use crate::indexer::{IndexStatus, Indexer};
use crate::mcp::{McpServer, McpStatus};
use crate::queue::Batch;
use crate::search::{SearchHit, Searcher};
use crate::store::{Store, VectorNode};
use crate::watcher::{Accept, Watcher};
use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

/// What `getStatus()` / the `core-status-update` event carry, same keys as the TS payload.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StatusPayload {
    pub status: &'static str,
    pub index_status: IndexStatus,
    pub mcp_status: McpStatus,
    pub mcp_port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_error: Option<String>,
    pub chunk_count: usize,
    pub model: String,
    pub embedding_provider: &'static str,
    pub dimension: usize,
    pub watch_dirs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DirInfo {
    pub path: String,
    pub paused: bool,
    pub chunk_count: usize,
}

enum Job {
    Sync { force: bool },
    Paths(Vec<PathBuf>),
    Delete(Vec<PathBuf>),
}

type Listener = Box<dyn Fn(StatusPayload) + Send + Sync>;

pub struct Engine {
    config_path: PathBuf,
    cfg: Arc<RwLock<Config>>,
    searcher: Searcher,
    indexer: Arc<Indexer>,
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    /// Jobs sent but not finished; `wait_idle` polls it.
    pending: Arc<AtomicUsize>,
    watcher: Mutex<Option<Watcher>>,
    mcp: tokio::sync::Mutex<McpServer>,
    /// Last known MCP state, readable without awaiting the async mutex.
    mcp_state: Mutex<(McpStatus, u16, Option<String>)>,
    listeners: Arc<Mutex<Vec<Listener>>>,
    me: Weak<Engine>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Engine {
    /// The real embedder for a config (downloads the model on first use).
    pub fn load_embedder(cfg: &Config) -> Result<Arc<dyn Embedder>> {
        std::fs::create_dir_all(cfg.models_dir())?;
        Ok(Arc::new(FastEmbedder::new(
            &cfg.local_model_name,
            &cfg.models_dir(),
        )?))
    }

    /// Loads config, opens the store (a model/dim change wipes it), starts the indexing worker,
    /// the watcher (if `autoIndexOnChange`) and queues the startup sync. MCP is started separately
    /// with `start_mcp` because it needs an async runtime.
    pub fn open(config_path: &Path, embedder: Arc<dyn Embedder>) -> Result<Arc<Engine>> {
        let cfg = Config::load(config_path);
        cfg.save(config_path)?;
        let (store, reset) = Store::open(&cfg.db_path(), embedder.model(), embedder.dim())?;
        if reset {
            tracing::info!("store created or model/dimension changed: full index");
        }
        let store = Arc::new(store);
        let cfg = Arc::new(RwLock::new(cfg));
        let indexer = Arc::new(Indexer::new(store.clone(), embedder.clone(), cfg.clone()));
        let searcher = Searcher {
            store,
            embedder,
            cfg: cfg.clone(),
        };
        let (tx, rx) = mpsc::channel::<Job>();
        let pending = Arc::new(AtomicUsize::new(0));
        let listeners: Arc<Mutex<Vec<Listener>>> = Arc::default();

        let engine = Arc::new_cyclic(|me| Engine {
            config_path: config_path.to_path_buf(),
            cfg: cfg.clone(),
            searcher: searcher.clone(),
            indexer: indexer.clone(),
            jobs: Mutex::new(Some(tx)),
            pending: pending.clone(),
            watcher: Mutex::new(None),
            mcp: tokio::sync::Mutex::new(McpServer::new(searcher)),
            mcp_state: Mutex::new((McpStatus::Stopped, cfg.read().unwrap().mcp_port, None)),
            listeners,
            me: me.clone(),
        });

        let weak = Arc::downgrade(&engine);
        indexer.on_status(move |_| {
            if let Some(e) = weak.upgrade() {
                e.notify();
            }
        });

        // One worker thread runs every index job in order; queued jobs are coalesced.
        let idx = indexer.clone();
        std::thread::Builder::new()
            .name("anamnesis-indexer".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    let mut jobs = vec![first];
                    jobs.extend(rx.try_iter());
                    let n = jobs.len();
                    let (mut sync, mut force, mut paths, mut deletes) =
                        (false, false, vec![], vec![]);
                    for j in jobs {
                        match j {
                            Job::Sync { force: f } => {
                                sync = true;
                                force |= f;
                            }
                            Job::Paths(p) => paths.extend(p),
                            Job::Delete(p) => deletes.extend(p),
                        }
                    }
                    let result = if sync {
                        idx.sync(force)
                    } else {
                        idx.delete_paths(&deletes).and_then(|_| {
                            if paths.is_empty() {
                                Ok(Default::default())
                            } else {
                                idx.index_paths(&paths)
                            }
                        })
                    };
                    match result {
                        Ok(r) => tracing::info!("index job done: {r:?}"),
                        Err(e) => tracing::error!("index job failed: {e:#}"),
                    }
                    if !sync && paths.is_empty() {
                        idx.set_status(IndexStatus::Idle);
                    }
                    pending.fetch_sub(n, Ordering::SeqCst);
                }
            })?;

        engine.restart_watcher()?;
        engine.send(Job::Sync { force: reset });
        Ok(engine)
    }

    fn send(&self, job: Job) {
        if let Some(tx) = self.jobs.lock().unwrap().as_ref() {
            self.pending.fetch_add(1, Ordering::SeqCst);
            if tx.send(job).is_err() {
                self.pending.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    fn notify(&self) {
        let p = self.status();
        for l in self.listeners.lock().unwrap().iter() {
            l(p.clone());
        }
    }

    /// (Re)creates the watcher for the current watch dirs, or removes it when auto-index is off.
    fn restart_watcher(&self) -> Result<()> {
        let cfg = self.config();
        let mut slot = self.watcher.lock().unwrap();
        let paused: Vec<PathBuf> = cfg
            .watch_dirs
            .iter()
            .map(PathBuf::from)
            .filter(|d| slot.as_ref().is_some_and(|w| w.is_dir_paused(d)))
            .collect();
        *slot = None;
        if !cfg.auto_index_on_change || cfg.watch_dirs.is_empty() {
            return Ok(());
        }
        let dirs: Vec<PathBuf> = cfg
            .watch_dirs
            .iter()
            .map(PathBuf::from)
            .filter(|d| d.is_dir())
            .collect();
        let filter = Arc::new(Filter::new(&cfg));
        let accept: Accept = Arc::new(move |p: &Path| filter.is_candidate(p));
        let (me_batch, me_queued) = (self.me.clone(), self.me.clone());
        let w = Watcher::start(
            &dirs,
            Duration::from_millis(cfg.indexing_debounce_ms),
            accept,
            move |b: Batch| {
                if let Some(e) = me_batch.upgrade() {
                    if !b.delete.is_empty() {
                        e.send(Job::Delete(b.delete));
                    }
                    if !b.modify.is_empty() {
                        e.send(Job::Paths(b.modify));
                    }
                }
            },
            move |count, delay| {
                if let Some(e) = me_queued.upgrade() {
                    if !matches!(
                        e.indexer.status(),
                        IndexStatus::Indexing { .. } | IndexStatus::Paused { .. }
                    ) {
                        let delay_ms = delay.as_millis() as u64;
                        e.indexer.set_status(IndexStatus::Queued {
                            count,
                            flush_at: now_ms() + delay_ms,
                            delay_ms,
                        });
                    }
                }
            },
        )?;
        for d in paused {
            w.pause_dir(&d);
        }
        *slot = Some(w);
        Ok(())
    }

    pub fn config_path(&self) -> PathBuf {
        self.config_path.clone()
    }

    pub fn config(&self) -> Config {
        self.cfg.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Applies a partial config from the UI, persists it and reacts: watch dirs, excludes and file
    /// types resync; chunking changes force a re-index; debounce/auto-index adjust the watcher;
    /// MCP port/enabled restart the server. Returns `true` when the embedding model changed and
    /// the engine must be restarted.
    pub async fn save_config(&self, partial: &serde_json::Value) -> Result<bool> {
        let old = self.config();
        let new = old.merged(partial)?;
        new.save(&self.config_path)?;
        *self.cfg.write().unwrap_or_else(|e| e.into_inner()) = new.clone();

        let filter_changed = old.watch_dirs != new.watch_dirs
            || old.exclude_patterns != new.exclude_patterns
            || old.dir_exclude_patterns != new.dir_exclude_patterns
            || old.file_types != new.file_types;
        let rechunk = old.chunk_size != new.chunk_size || old.chunk_overlap != new.chunk_overlap;
        if filter_changed || old.auto_index_on_change != new.auto_index_on_change {
            self.restart_watcher()?;
        } else if old.indexing_debounce_ms != new.indexing_debounce_ms {
            if let Some(w) = self.watcher.lock().unwrap().as_ref() {
                w.set_debounce(Duration::from_millis(new.indexing_debounce_ms));
            }
        }
        if filter_changed || rechunk {
            self.send(Job::Sync { force: rechunk });
        }
        if !new.mcp_enabled && old.mcp_enabled {
            self.stop_mcp().await;
        } else if new.mcp_enabled
            && old.mcp_port != new.mcp_port
            && self.status().mcp_status == McpStatus::Running
        {
            self.start_mcp().await?;
        }
        self.mcp_state.lock().unwrap().1 = new.mcp_port;
        self.notify();
        Ok(old.local_model_name != new.local_model_name)
    }

    pub fn status(&self) -> StatusPayload {
        let cfg = self.config();
        let (mcp_status, mcp_port, mcp_error) = self.mcp_state.lock().unwrap().clone();
        StatusPayload {
            status: "running",
            index_status: self.indexer.status(),
            mcp_status,
            mcp_port,
            mcp_error,
            chunk_count: self.searcher.store.chunk_count().unwrap_or(0),
            model: self.searcher.embedder.model().to_string(),
            embedding_provider: "local",
            dimension: self.searcher.embedder.dim(),
            watch_dirs: cfg.watch_dirs,
        }
    }

    /// Called with a fresh payload on every index or MCP status change.
    pub fn on_status(&self, f: impl Fn(StatusPayload) + Send + Sync + 'static) {
        self.listeners.lock().unwrap().push(Box::new(f));
    }

    pub fn reindex(&self) {
        self.send(Job::Sync { force: true });
    }

    pub fn cancel(&self) {
        self.indexer.cancel();
    }

    pub fn pause(&self) {
        self.indexer.pause();
        if let Some(w) = self.watcher.lock().unwrap().as_ref() {
            for d in &self.config().watch_dirs {
                w.pause_dir(Path::new(d));
            }
        }
        self.notify();
    }

    pub fn resume(&self) {
        self.indexer.resume();
        if let Some(w) = self.watcher.lock().unwrap().as_ref() {
            w.resume_all();
        }
        self.notify();
    }

    pub fn flush(&self) {
        if let Some(w) = self.watcher.lock().unwrap().as_ref() {
            w.flush_now();
        }
    }

    pub fn dirs(&self) -> Result<Vec<DirInfo>> {
        let cfg = self.config();
        let counts = self.searcher.store.file_chunk_counts()?;
        let w = self.watcher.lock().unwrap();
        Ok(cfg
            .watch_dirs
            .iter()
            .map(|d| DirInfo {
                path: d.clone(),
                paused: w.as_ref().is_some_and(|w| w.is_dir_paused(Path::new(d))),
                chunk_count: counts
                    .iter()
                    .filter(|(p, _)| Path::new(p).starts_with(d))
                    .map(|(_, n)| n)
                    .sum(),
            })
            .collect())
    }

    pub fn pause_dir(&self, dir: &str) {
        if let Some(w) = self.watcher.lock().unwrap().as_ref() {
            w.pause_dir(Path::new(dir));
        }
    }

    pub fn resume_dir(&self, dir: &str) {
        if let Some(w) = self.watcher.lock().unwrap().as_ref() {
            w.resume_dir(Path::new(dir));
        }
    }

    pub fn reindex_dir(&self, dir: &str) {
        self.send(Job::Paths(vec![PathBuf::from(dir)]));
    }

    pub fn searcher(&self) -> Searcher {
        self.searcher.clone()
    }

    pub fn search(&self, query: &str, limit: Option<usize>) -> Result<Vec<SearchHit>> {
        self.searcher.search(query, limit)
    }

    pub fn vectors(&self, limit: usize) -> Result<Vec<VectorNode>> {
        self.searcher.store.vector_sample(limit.min(5000))
    }

    pub async fn start_mcp(&self) -> Result<()> {
        let port = self.config().mcp_port;
        let mut m = self.mcp.lock().await;
        let r = m.start(port).await;
        *self.mcp_state.lock().unwrap() = (m.status(), m.port(), m.error());
        drop(m);
        self.notify();
        r
    }

    pub async fn stop_mcp(&self) {
        let mut m = self.mcp.lock().await;
        m.stop().await;
        *self.mcp_state.lock().unwrap() = (m.status(), self.config().mcp_port, None);
        drop(m);
        self.notify();
    }

    /// Stops the watcher, cancels indexing and the MCP server. The engine is unusable after.
    pub async fn shutdown(&self) {
        *self.watcher.lock().unwrap() = None;
        self.indexer.cancel();
        *self.jobs.lock().unwrap() = None;
        self.stop_mcp().await;
    }

    /// Blocks until the indexing worker has no queued or running job (tests, headless start).
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let t = Instant::now();
        while t.elapsed() < timeout {
            if self.pending.load(Ordering::SeqCst) == 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}
