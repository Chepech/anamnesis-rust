//! Walk → parse → chunk → embed → write, with three dedup levels:
//!
//! 1. mtime unchanged → skip without reading.
//! 2. content hash unchanged → touch mtime only.
//! 3. chunk embed-text hash already stored (any file) → reuse its vector, no ORT call.
//!
//! New chunks from many files are embedded together in batches of `EMBED_BATCH`.

use crate::chunker::split_markdown;
use crate::config::Config;
use crate::embed::Embedder;
use crate::filter::Filter;
use crate::parsers::{self, wikilinks};
use crate::store::{stem_of, ChunkRow, FileRecord, Store, MAX_BACKLINKS};
use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// Files parsed and written per group; new chunks within a group share embed calls.
const FILE_GROUP: usize = 64;

pub const EMBED_BATCH: usize = 64;
pub const BREADCRUMB_MAX_CHARS: usize = 150;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum IndexStatus {
    Idle,
    Queued {
        count: usize,
        #[serde(rename = "flushAt")]
        flush_at: u64,
        #[serde(rename = "delayMs")]
        delay_ms: u64,
    },
    Indexing {
        current: usize,
        total: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Paused {
        current: usize,
        total: usize,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    /// Files whose chunks were (re)written.
    pub indexed: usize,
    /// Files skipped because mtime was unchanged.
    pub unchanged: usize,
    /// Files whose mtime changed but content hash did not.
    pub touched: usize,
    pub deleted: usize,
    pub failed: usize,
    /// Chunks sent to the embedder.
    pub embedded: usize,
    /// Chunks whose vector was reused by hash.
    pub reused: usize,
}

/// `[title] > [context] :: text`, breadcrumb capped at 150 chars, backlinks appended when given.
pub fn embed_text(title: &str, context_path: &str, text: &str, backlinks: &[String]) -> String {
    let crumb = if context_path.is_empty() {
        format!("[{title}]")
    } else {
        format!("[{title}] > [{context_path}]")
    };
    let crumb = if crumb.chars().count() > BREADCRUMB_MAX_CHARS {
        crumb
            .chars()
            .take(BREADCRUMB_MAX_CHARS - 3)
            .chain("...".chars())
            .collect()
    } else {
        crumb
    };
    let suffix = if backlinks.is_empty() {
        String::new()
    } else {
        format!(" Linked from: {}", backlinks.join(", "))
    };
    format!("{crumb} :: {text}{suffix}")
}

type Listener = Box<dyn Fn(&IndexStatus) + Send + Sync>;

pub struct Indexer {
    store: Arc<Store>,
    embedder: Arc<dyn Embedder>,
    cfg: Arc<RwLock<Config>>,
    status: Mutex<IndexStatus>,
    listeners: Mutex<Vec<Listener>>,
    paused: AtomicBool,
    cancelled: AtomicBool,
    /// Serializes jobs: a watcher batch never interleaves with a full sync.
    job: Mutex<()>,
}

struct Changed {
    path: PathBuf,
    mtime_ns: i64,
    hash: String,
}

/// Tags plus (chunk, embed text, embed hash) per chunk.
type Parsed = (String, Vec<(crate::chunker::Chunk, String, String)>);

struct Prepared<'a> {
    changed: &'a Changed,
    doc: Option<Parsed>,
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i64)
}

fn key(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

impl Indexer {
    pub fn new(
        store: Arc<Store>,
        embedder: Arc<dyn Embedder>,
        cfg: Arc<RwLock<Config>>,
    ) -> Indexer {
        Indexer {
            store,
            embedder,
            cfg,
            status: Mutex::new(IndexStatus::Idle),
            listeners: Mutex::new(vec![]),
            paused: false.into(),
            cancelled: false.into(),
            job: Mutex::new(()),
        }
    }

    /// Registers a listener called on every status change (tray, UI events, logs).
    pub fn on_status(&self, f: impl Fn(&IndexStatus) + Send + Sync + 'static) {
        self.listeners.lock().unwrap().push(Box::new(f));
    }

    pub fn status(&self) -> IndexStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn set_status(&self, s: IndexStatus) {
        *self.status.lock().unwrap() = s.clone();
        for l in self.listeners.lock().unwrap().iter() {
            l(&s);
        }
    }

    fn cfg(&self) -> Config {
        self.cfg.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Brings the store in line with the watch dirs: index new/changed files, purge files that
    /// vanished or became excluded. `force` wipes everything first (manual full re-index).
    pub fn sync(&self, force: bool) -> Result<Report> {
        let _job = self.job.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = self.cfg();
        let filter = Filter::new(&cfg);
        if force {
            self.store.clear()?;
        }
        let files = walk(
            &filter,
            &cfg.watch_dirs.iter().map(PathBuf::from).collect::<Vec<_>>(),
        );
        let on_disk: HashSet<String> = files.iter().map(|p| key(p)).collect();
        let mut report = Report::default();
        for p in self.store.indexed_paths()? {
            if !on_disk.contains(&p) {
                report.deleted += self.store.delete_path(&p)?;
            }
        }
        self.run(files, &cfg, report)
    }

    /// Indexes the given files; directories are expanded, non-indexable paths ignored.
    pub fn index_paths(&self, paths: &[PathBuf]) -> Result<Report> {
        let _job = self.job.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = self.cfg();
        let filter = Filter::new(&cfg);
        let mut files: Vec<PathBuf> = paths
            .iter()
            .flat_map(|p| {
                if p.is_dir() {
                    walk(&filter, std::slice::from_ref(p))
                } else {
                    vec![p.clone()]
                }
            })
            .filter(|p| p.is_file() && filter.is_indexable(p))
            .collect();
        files.sort();
        files.dedup();
        self.run(files, &cfg, Report::default())
    }

    /// Removes files (or whole directories) from the index.
    pub fn delete_paths(&self, paths: &[PathBuf]) -> Result<Report> {
        let _job = self.job.lock().unwrap_or_else(|e| e.into_inner());
        let mut report = Report::default();
        for p in paths {
            report.deleted += self.store.delete_path(&key(p))?;
        }
        Ok(report)
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.resume();
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Blocks while paused. Returns false when the job was cancelled.
    fn checkpoint(&self, current: usize, total: usize) -> bool {
        if self.is_paused() {
            self.set_status(IndexStatus::Paused { current, total });
            // ponytail: 50 ms sleep loop; a Condvar if pause latency ever matters.
            while self.is_paused() && !self.cancelled.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
            }
            self.set_status(IndexStatus::Indexing {
                current,
                total,
                label: None,
            });
        }
        !self.cancelled.load(Ordering::SeqCst)
    }

    fn run(&self, files: Vec<PathBuf>, cfg: &Config, report: Report) -> Result<Report> {
        self.cancelled.store(false, Ordering::SeqCst);
        let result = self.pipeline(files, cfg, report);
        self.cancelled.store(false, Ordering::SeqCst);
        self.paused.store(false, Ordering::SeqCst);
        match &result {
            Ok(_) => self.set_status(IndexStatus::Idle),
            Err(e) => self.set_status(IndexStatus::Error {
                message: e.to_string(),
            }),
        }
        result
    }

    fn pipeline(&self, files: Vec<PathBuf>, cfg: &Config, mut report: Report) -> Result<Report> {
        let total = files.len();
        self.set_status(IndexStatus::Indexing {
            current: 0,
            total,
            label: None,
        });

        // Pass A: change detection (dedup levels 1 and 2) and wikilinks for every changed file,
        // so backlinks are complete before any embed text is built.
        let mut changed = vec![];
        for (i, path) in files.into_iter().enumerate() {
            if !self.checkpoint(i, total) {
                return Ok(report);
            }
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let (k, mtime) = (key(&path), mtime_ns(&meta));
            let state = self.store.file_state(&k)?;
            if state.as_ref().is_some_and(|s| s.mtime_ns == mtime) {
                report.unchanged += 1;
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let hash = blake3::hash(&bytes).to_hex().to_string();
            if state.is_some_and(|s| s.content_hash == hash) {
                self.store.touch_file(&k, mtime)?;
                report.touched += 1;
                continue;
            }
            if parsers::extension(&path) == "md" {
                self.store
                    .set_links(&k, &wikilinks(&String::from_utf8_lossy(&bytes)))?;
            }
            changed.push(Changed {
                path,
                mtime_ns: mtime,
                hash,
            });
        }

        // Pass B: parse in parallel, embed only chunk texts not already stored (dedup level 3),
        // several files per embed call, one transaction per file.
        let done_before = total - changed.len();
        for (g, group) in changed.chunks(FILE_GROUP).enumerate() {
            let current = done_before + g * FILE_GROUP;
            if !self.checkpoint(current, total) {
                return Ok(report);
            }
            self.set_status(IndexStatus::Indexing {
                current,
                total,
                label: group.first().map(|c| stem_of(&key(&c.path))),
            });
            let prepared: Vec<Prepared> = group
                .par_iter()
                .map(|c| Prepared {
                    doc: self.prepare(&c.path, cfg),
                    changed: c,
                })
                .collect();

            let hashes: Vec<String> = prepared
                .iter()
                .flat_map(|p| {
                    p.doc
                        .iter()
                        .flat_map(|(_, cs)| cs.iter().map(|c| c.2.clone()))
                })
                .collect();
            let mut vectors = self.store.vectors_by_hash(&hashes)?;
            let mut missing: Vec<(String, String)> = vec![];
            let mut seen: HashSet<&str> = vectors.keys().map(String::as_str).collect();
            for p in &prepared {
                for (_, text, hash) in p.doc.iter().flat_map(|(_, cs)| cs) {
                    if seen.insert(hash) {
                        missing.push((hash.clone(), text.clone()));
                    }
                }
            }
            report.reused += hashes.len() - missing.len();
            for batch in missing.chunks(EMBED_BATCH) {
                if !self.checkpoint(current, total) {
                    return Ok(report);
                }
                let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
                let embedded = self.embedder.embed(&texts)?;
                anyhow::ensure!(
                    embedded.len() == batch.len(),
                    "embedder returned {} vectors for {} texts",
                    embedded.len(),
                    batch.len()
                );
                report.embedded += batch.len();
                vectors.extend(batch.iter().map(|(h, _)| h.clone()).zip(embedded));
            }

            for p in prepared {
                // Failed files are recorded with their hash and no chunks, so an unchanged
                // broken PDF is not re-parsed on every start.
                let (tags, chunks) = match p.doc {
                    Some(d) => {
                        report.indexed += 1;
                        d
                    }
                    None => {
                        report.failed += 1;
                        (String::new(), vec![])
                    }
                };
                let rows = chunks
                    .into_iter()
                    .map(|(c, _, hash)| ChunkRow {
                        chunk_index: c.chunk_index,
                        heading: c.heading,
                        context_path: c.context_path,
                        text: c.text,
                        vector: vectors[&hash].clone(),
                        embed_hash: hash,
                    })
                    .collect();
                self.store
                    .replace_file(&FileRecord {
                        path: key(&p.changed.path),
                        mtime_ns: p.changed.mtime_ns,
                        content_hash: p.changed.hash.clone(),
                        tags,
                        chunks: rows,
                    })
                    .with_context(|| format!("writing {}", p.changed.path.display()))?;
            }
        }
        Ok(report)
    }

    /// Reads, parses and chunks one file. `None` when it cannot be parsed.
    fn prepare(&self, path: &Path, cfg: &Config) -> Option<Parsed> {
        let bytes = std::fs::read(path).ok()?;
        let doc = parsers::parse(path, &bytes)?;
        let title = stem_of(&key(path));
        let backlinks = self
            .store
            .backlink_titles(&title, MAX_BACKLINKS)
            .unwrap_or_default();
        let chunks = split_markdown(&doc.text, cfg.chunk_size, cfg.chunk_overlap)
            .into_iter()
            .map(|c| {
                let text = embed_text(
                    &title,
                    &c.context_path,
                    &c.text,
                    if c.chunk_index == 0 { &backlinks } else { &[] },
                );
                let hash = blake3::hash(text.as_bytes()).to_hex().to_string();
                (c, text, hash)
            })
            .collect();
        Some((doc.tags.join(", "), chunks))
    }
}

/// All indexable files under `roots`, sorted. Dot dirs and excluded dirs are pruned, not walked.
fn walk(filter: &Filter, roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = vec![];
    for root in roots {
        let walker = ignore::WalkBuilder::new(root)
            .standard_filters(false)
            .filter_entry({
                let root = root.clone();
                move |e| {
                    e.path() == root
                        || !e.file_type().is_some_and(|t| t.is_dir())
                        || !e.file_name().to_string_lossy().starts_with('.')
                }
            })
            .build();
        for e in walker.flatten() {
            if e.file_type().is_some_and(|t| t.is_file()) && filter.is_indexable(e.path()) {
                out.push(e.into_path());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::embed::HashEmbedder;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// HashEmbedder that records calls, can fail on demand and can block until released.
    pub struct Counting {
        inner: HashEmbedder,
        pub calls: AtomicUsize,
        pub texts: Mutex<Vec<String>>,
        pub fail: std::sync::atomic::AtomicBool,
    }

    impl Counting {
        pub fn new() -> Arc<Counting> {
            Arc::new(Counting {
                inner: HashEmbedder::new(32),
                calls: 0.into(),
                texts: Mutex::new(vec![]),
                fail: false.into(),
            })
        }
        pub fn embedded(&self) -> usize {
            self.texts.lock().unwrap().len()
        }
    }

    impl Embedder for Counting {
        fn model(&self) -> &str {
            "counting"
        }
        fn dim(&self) -> usize {
            32
        }
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            if self.fail.load(Ordering::SeqCst) {
                anyhow::bail!("embedder exploded");
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.texts.lock().unwrap().extend(texts.iter().cloned());
            self.inner.embed(texts)
        }
    }

    pub struct Fixture {
        pub dir: tempfile::TempDir,
        pub store: Arc<Store>,
        pub emb: Arc<Counting>,
        pub cfg: Arc<RwLock<Config>>,
        pub idx: Indexer,
    }

    impl Fixture {
        pub fn new() -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(Store::open_in_memory("counting", 32).unwrap());
            let emb = Counting::new();
            let cfg = Arc::new(RwLock::new(Config {
                watch_dirs: vec![dir.path().to_string_lossy().into()],
                ..Config::default()
            }));
            let idx = Indexer::new(store.clone(), emb.clone(), cfg.clone());
            Fixture {
                dir,
                store,
                emb,
                cfg,
                idx,
            }
        }
        /// `rel` uses `/`; joined per component so Windows paths use `\` throughout.
        pub fn path(&self, rel: &str) -> PathBuf {
            rel.split('/')
                .fold(self.dir.path().to_path_buf(), |p, c| p.join(c))
        }
        pub fn write(&self, rel: &str, body: &str) -> PathBuf {
            let p = self.path(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            p
        }
        /// Changes mtime without changing bytes.
        pub fn touch(&self, rel: &str) {
            let p = self.path(rel);
            let f = std::fs::File::options().write(true).open(&p).unwrap();
            let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
            f.set_modified(later).unwrap();
        }
        pub fn key(&self, rel: &str) -> String {
            self.path(rel).to_string_lossy().into_owned()
        }
    }

    #[test]
    fn embed_text_formats_breadcrumb_and_backlinks() {
        assert_eq!(embed_text("Note", "", "body", &[]), "[Note] :: body");
        assert_eq!(
            embed_text("Note", "A > B", "body", &[]),
            "[Note] > [A > B] :: body"
        );
        assert_eq!(
            embed_text("N", "", "b", &["X".into(), "Y".into()]),
            "[N] :: b Linked from: X, Y"
        );
        let long = "c".repeat(300);
        let t = embed_text("N", &long, "b", &[]);
        let crumb = t.split(" :: ").next().unwrap();
        assert_eq!(crumb.chars().count(), BREADCRUMB_MAX_CHARS);
        assert!(crumb.ends_with("..."));
    }

    #[test]
    fn sync_indexes_supported_files_and_skips_the_rest() {
        let f = Fixture::new();
        f.write("a.md", "# A\nalpha text");
        f.write("sub/b.md", "beta text");
        f.write("skip.txt", "nope");
        f.write(".obsidian/c.md", "hidden");
        let r = f.idx.sync(false).unwrap();
        assert_eq!(r.indexed, 2);
        assert_eq!(
            f.store.indexed_paths().unwrap(),
            vec![f.key("a.md"), f.key("sub/b.md")]
        );
        assert_eq!(f.idx.status(), IndexStatus::Idle);
    }

    #[test]
    fn second_sync_with_no_changes_does_no_work() {
        let f = Fixture::new();
        f.write("a.md", "alpha");
        f.idx.sync(false).unwrap();
        let calls = f.emb.calls.load(Ordering::SeqCst);
        let r = f.idx.sync(false).unwrap();
        assert_eq!((r.indexed, r.unchanged, r.embedded), (0, 1, 0));
        assert_eq!(f.emb.calls.load(Ordering::SeqCst), calls);
    }

    #[test]
    fn touched_file_with_same_content_is_not_reembedded() {
        let f = Fixture::new();
        f.write("a.md", "alpha");
        f.idx.sync(false).unwrap();
        let before = f.store.file_state(&f.key("a.md")).unwrap().unwrap();
        f.touch("a.md");
        let r = f.idx.index_paths(&[f.path("a.md")]).unwrap();
        assert_eq!((r.touched, r.indexed, r.embedded), (1, 0, 0));
        let after = f.store.file_state(&f.key("a.md")).unwrap().unwrap();
        assert_ne!(
            before.mtime_ns, after.mtime_ns,
            "mtime recorded so the next pass skips cheaply"
        );
        assert_eq!(before.content_hash, after.content_hash);
    }

    #[test]
    fn editing_one_section_only_embeds_changed_chunks() {
        let f = Fixture::new();
        let big = |s: &str| {
            format!(
                "# One\n{}\n# Two\n{}\n# Three\n{}",
                "a ".repeat(100),
                s,
                "c ".repeat(100)
            )
        };
        f.write("a.md", &big(&"b ".repeat(100)));
        f.idx.sync(false).unwrap();
        let first = f.emb.embedded();
        assert_eq!(first, 3);
        f.write("a.md", &big("changed middle section"));
        let r = f.idx.index_paths(&[f.path("a.md")]).unwrap();
        assert_eq!((r.indexed, r.embedded, r.reused), (1, 1, 2));
        assert_eq!(f.store.chunk_count().unwrap(), 3);
    }

    #[test]
    fn identical_chunks_across_files_are_embedded_once() {
        let f = Fixture::new();
        // Same title (stem) + same text ⇒ same embed text, e.g. copies in two folders.
        f.write("x/Note.md", "shared paragraph");
        f.write("y/Note.md", "shared paragraph");
        let r = f.idx.sync(false).unwrap();
        assert_eq!((r.indexed, r.embedded, r.reused), (2, 1, 1));
        assert_eq!(f.store.chunk_count().unwrap(), 2);
    }

    #[test]
    fn many_small_files_share_embed_batches() {
        let f = Fixture::new();
        for i in 0..100 {
            f.write(&format!("n{i}.md"), &format!("note number {i}"));
        }
        f.idx.sync(false).unwrap();
        assert_eq!(f.emb.embedded(), 100);
        assert_eq!(
            f.emb.calls.load(Ordering::SeqCst),
            2,
            "100 chunks / batch 64 = 2 calls, not 100"
        );
    }

    #[test]
    fn sync_purges_deleted_and_newly_excluded_files() {
        let f = Fixture::new();
        f.write("keep.md", "k");
        f.write("gone.md", "g");
        f.write("private/p.md", "p");
        f.idx.sync(false).unwrap();
        std::fs::remove_file(f.path("gone.md")).unwrap();
        f.cfg
            .write()
            .unwrap()
            .exclude_patterns
            .push("private".into());
        let r = f.idx.sync(false).unwrap();
        assert_eq!(r.deleted, 2);
        assert_eq!(f.store.indexed_paths().unwrap(), vec![f.key("keep.md")]);
    }

    #[test]
    fn sync_purges_files_of_removed_watch_dirs() {
        let f = Fixture::new();
        f.write("a.md", "a");
        f.idx.sync(false).unwrap();
        f.cfg.write().unwrap().watch_dirs.clear();
        f.idx.sync(false).unwrap();
        assert!(f.store.indexed_paths().unwrap().is_empty());
    }

    #[test]
    fn forced_sync_reembeds_everything() {
        let f = Fixture::new();
        f.write("a.md", "alpha");
        f.idx.sync(false).unwrap();
        let r = f.idx.sync(true).unwrap();
        assert_eq!((r.indexed, r.embedded), (1, 1));
    }

    #[test]
    fn backlinks_feed_embed_text_and_importance_and_update_incrementally() {
        let f = Fixture::new();
        f.write("Source.md", "points to [[Target]]");
        f.write("Target.md", "the target");
        f.idx.sync(false).unwrap();
        assert!(f
            .emb
            .texts
            .lock()
            .unwrap()
            .iter()
            .any(|t| t == "[Target] :: the target Linked from: Source"));
        let imp = |f: &Fixture| {
            let (id, _) = f
                .store
                .knn(
                    &HashEmbedder::new(32).embed(&["the target".into()]).unwrap()[0],
                    10,
                )
                .unwrap()
                .into_iter()
                .find(|(id, _)| {
                    f.store.hits(&[*id]).unwrap()[id]
                        .file_path
                        .ends_with("Target.md")
                })
                .unwrap();
            f.store.hits(&[id]).unwrap()[&id].importance_score
        };
        assert_eq!(imp(&f), 1);
        f.write("Source.md", "no more links");
        f.idx.index_paths(&[f.path("Source.md")]).unwrap();
        assert_eq!(imp(&f), 0, "importance is live, not frozen at index time");
    }

    #[test]
    fn full_sync_sees_backlinks_from_files_processed_later() {
        let f = Fixture::new();
        // "a" sorts before "z": a's embed text needs z's link, collected in the links pre-pass.
        f.write("a.md", "first");
        f.write("z.md", "[[a]]");
        f.idx.sync(false).unwrap();
        assert!(f
            .emb
            .texts
            .lock()
            .unwrap()
            .iter()
            .any(|t| t == "[a] :: first Linked from: z"));
    }

    #[test]
    fn unparsable_file_is_counted_as_failed_and_others_still_index() {
        let f = Fixture::new();
        f.write("bad.pdf", "%PDF-1.4 garbage");
        f.write("good.md", "fine");
        let r = f.idx.sync(false).unwrap();
        assert_eq!((r.indexed, r.failed), (1, 1));
        assert_eq!(f.idx.status(), IndexStatus::Idle);
    }

    #[test]
    fn index_paths_expands_directories_and_ignores_outsiders() {
        let f = Fixture::new();
        f.write("d/one.md", "1");
        f.write("d/two.md", "2");
        let outside = tempfile::tempdir().unwrap();
        let o = outside.path().join("x.md");
        std::fs::write(&o, "x").unwrap();
        let r = f.idx.index_paths(&[f.path("d"), o]).unwrap();
        assert_eq!(r.indexed, 2);
    }

    #[test]
    fn delete_paths_removes_files_and_directories() {
        let f = Fixture::new();
        f.write("d/one.md", "1");
        f.write("two.md", "2");
        f.idx.sync(false).unwrap();
        let r = f
            .idx
            .delete_paths(&[f.path("d"), f.path("two.md")])
            .unwrap();
        assert_eq!(r.deleted, 2);
        assert_eq!(f.store.chunk_count().unwrap(), 0);
    }

    #[test]
    fn embedder_failure_sets_error_status_and_writes_nothing() {
        let f = Fixture::new();
        f.write("a.md", "alpha");
        f.emb.fail.store(true, Ordering::SeqCst);
        assert!(f.idx.sync(false).is_err());
        assert!(
            matches!(f.idx.status(), IndexStatus::Error { message } if message.contains("exploded"))
        );
        assert_eq!(f.store.chunk_count().unwrap(), 0);
        assert_eq!(
            f.store.file_state(&f.key("a.md")).unwrap(),
            None,
            "no hash stored, so a retry re-indexes"
        );
        f.emb.fail.store(false, Ordering::SeqCst);
        assert_eq!(f.idx.sync(false).unwrap().indexed, 1);
    }

    #[test]
    fn status_reports_progress_during_indexing() {
        let f = Fixture::new();
        for i in 0..5 {
            f.write(&format!("n{i}.md"), "x");
        }
        let seen = Arc::new(Mutex::new(vec![]));
        let s2 = seen.clone();
        f.idx
            .on_status(move |st| s2.lock().unwrap().push(st.clone()));
        f.idx.sync(false).unwrap();
        let seen = seen.lock().unwrap();
        assert!(
            seen.iter()
                .any(|s| matches!(s, IndexStatus::Indexing { total: 5, .. })),
            "{seen:?}"
        );
        assert_eq!(seen.last(), Some(&IndexStatus::Idle));
    }

    #[test]
    fn status_serializes_like_the_ts_daemon() {
        let q = serde_json::to_value(IndexStatus::Queued {
            count: 2,
            flush_at: 10,
            delay_ms: 5,
        })
        .unwrap();
        assert_eq!(
            q,
            serde_json::json!({"state": "queued", "count": 2, "flushAt": 10, "delayMs": 5})
        );
        assert_eq!(
            serde_json::to_value(IndexStatus::Idle).unwrap(),
            serde_json::json!({"state": "idle"})
        );
    }

    #[test]
    fn cancel_stops_a_running_sync() {
        let f = Arc::new(Fixture::new());
        for i in 0..300 {
            f.write(&format!("n{i}.md"), &format!("note {i}"));
        }
        f.idx.pause();
        let f2 = f.clone();
        let h = std::thread::spawn(move || f2.idx.sync(false));
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            matches!(f.idx.status(), IndexStatus::Paused { .. }),
            "{:?}",
            f.idx.status()
        );
        f.idx.cancel();
        let r = h.join().unwrap().unwrap();
        assert!(r.indexed < 300);
        assert!(!f.idx.is_paused());
        assert_eq!(f.idx.status(), IndexStatus::Idle);
    }

    #[test]
    fn pause_then_resume_completes() {
        let f = Arc::new(Fixture::new());
        for i in 0..70 {
            f.write(&format!("n{i}.md"), &format!("note {i}"));
        }
        f.idx.pause();
        let f2 = f.clone();
        let h = std::thread::spawn(move || f2.idx.sync(false));
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert_eq!(
            f.store.chunk_count().unwrap(),
            0,
            "nothing written while paused"
        );
        f.idx.resume();
        assert_eq!(h.join().unwrap().unwrap().indexed, 70);
    }

    #[test]
    fn fixture_paths_are_absolute() {
        let f = Fixture::new();
        assert!(Path::new(&f.key("a.md")).is_absolute());
    }
}
