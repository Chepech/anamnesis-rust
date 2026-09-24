//! OS file events (notify: inotify / FSEvents / ReadDirectoryChangesW, no polling) → PendingQueue
//! → batches for the indexer. A path that still exists is a modify, a missing one a delete, which
//! covers create/modify/rename/remove on every platform without per-OS event decoding.

use crate::queue::{Batch, Op, PendingQueue};
use anyhow::Result;
use notify::event::{EventKind, ModifyKind};
use notify::{RecursiveMode, Watcher as _};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

pub type Accept = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

enum Cmd {
    /// A path plus whether the event can mean "a directory appeared" (create or rename).
    Event(PathBuf, bool),
    Flush,
    Debounce(Duration),
}

pub struct Watcher {
    inner: Mutex<notify::RecommendedWatcher>,
    tx: mpsc::Sender<Cmd>,
    paused: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Watcher {
    pub fn start(
        dirs: &[PathBuf],
        debounce: Duration,
        accept: Accept,
        on_batch: impl Fn(Batch) + Send + 'static,
        on_queued: impl Fn(usize, Duration) + Send + 'static,
    ) -> Result<Watcher> {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let paused: Arc<Mutex<HashSet<PathBuf>>> = Arc::default();
        let (etx, p2, roots) = (tx.clone(), paused.clone(), dirs.to_vec());
        let inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            let structural = match ev.kind {
                EventKind::Access(_) | EventKind::Modify(ModifyKind::Metadata(_)) | EventKind::Other => return,
                EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_)) => true,
                _ => false,
            };
            let paused = p2.lock().unwrap();
            for p in ev.paths {
                if !roots.contains(&p) && accept(&p) && !paused.iter().any(|d| p.starts_with(d)) {
                    let _ = etx.send(Cmd::Event(p, structural));
                }
            }
        })?;
        let w = Watcher { inner: Mutex::new(inner), tx, paused };
        for d in dirs {
            w.watch_dir(d)?;
        }
        std::thread::Builder::new().name("anamnesis-watch-queue".into()).spawn(move || {
            let (mut q, mut debounce) = (PendingQueue::default(), debounce);
            loop {
                let msg = match q.deadline() {
                    Some(d) => rx.recv_timeout(d.saturating_duration_since(Instant::now())),
                    None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
                };
                match msg {
                    Ok(Cmd::Event(p, structural)) => {
                        // Existing path = modify, missing = delete. Directories only count when they
                        // appear (create/rename-in); their own metadata churn would re-walk them.
                        let op = match std::fs::metadata(&p) {
                            Ok(m) if m.is_dir() && !structural => continue,
                            Ok(_) => Op::Modify,
                            Err(_) => Op::Delete,
                        };
                        q.push(p, op, Instant::now(), debounce);
                        on_queued(q.len(), debounce);
                    }
                    Ok(Cmd::Flush) => {
                        let b = q.take_all();
                        if !b.is_empty() {
                            on_batch(b);
                        }
                    }
                    Ok(Cmd::Debounce(d)) => debounce = d,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Some(b) = q.take_due(Instant::now()) {
                            on_batch(b);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })?;
        Ok(w)
    }

    pub fn watch_dir(&self, dir: &Path) -> Result<()> {
        self.inner.lock().unwrap().watch(dir, RecursiveMode::Recursive)?;
        Ok(())
    }

    pub fn pause_dir(&self, dir: &Path) {
        self.paused.lock().unwrap().insert(dir.to_path_buf());
    }

    pub fn resume_dir(&self, dir: &Path) {
        self.paused.lock().unwrap().remove(dir);
    }

    pub fn resume_all(&self) {
        self.paused.lock().unwrap().clear();
    }

    pub fn is_dir_paused(&self, dir: &Path) -> bool {
        self.paused.lock().unwrap().contains(dir)
    }

    pub fn set_debounce(&self, d: Duration) {
        let _ = self.tx.send(Cmd::Debounce(d));
    }

    /// Delivers whatever is pending immediately ("Index now").
    pub fn flush_now(&self) {
        let _ = self.tx.send(Cmd::Flush);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const WAIT: Duration = Duration::from_secs(5);

    struct W {
        dir: tempfile::TempDir,
        rx: mpsc::Receiver<Batch>,
        queued: mpsc::Receiver<usize>,
        w: Watcher,
    }

    fn start(debounce_ms: u64) -> W {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let (qtx, queued) = mpsc::channel();
        let accept: Accept = Arc::new(|p: &Path| p.extension().is_none_or(|e| e == "md"));
        let w = Watcher::start(
            &[dir.path().to_path_buf()],
            Duration::from_millis(debounce_ms),
            accept,
            move |b| tx.send(b).unwrap(),
            move |n, _| {
                let _ = qtx.send(n);
            },
        )
        .unwrap();
        W { dir, rx, queued, w }
    }

    #[test]
    fn created_file_arrives_as_modify_after_debounce() {
        let t = start(150);
        let p = t.dir.path().join("a.md");
        std::fs::write(&p, "x").unwrap();
        let b = t.rx.recv_timeout(WAIT).unwrap();
        assert_eq!(b.modify, vec![p]);
        assert!(t.queued.try_recv().unwrap() >= 1, "queued count reported before the flush");
    }

    #[test]
    fn removed_file_arrives_as_delete() {
        let t = start(150);
        let p = t.dir.path().join("a.md");
        std::fs::write(&p, "x").unwrap();
        t.rx.recv_timeout(WAIT).unwrap();
        std::fs::remove_file(&p).unwrap();
        let b = t.rx.recv_timeout(WAIT).unwrap();
        assert_eq!(b.delete, vec![p]);
        assert!(b.modify.is_empty());
    }

    #[test]
    fn burst_of_writes_collapses_into_one_entry() {
        let t = start(300);
        let p = t.dir.path().join("a.md");
        for i in 0..20 {
            std::fs::write(&p, format!("v{i}")).unwrap();
        }
        let b = t.rx.recv_timeout(WAIT).unwrap();
        assert_eq!(b.modify, vec![p]);
        assert!(t.rx.recv_timeout(Duration::from_millis(600)).is_err(), "no second batch");
    }

    #[test]
    fn rejected_paths_never_queue() {
        let t = start(150);
        std::fs::write(t.dir.path().join("a.tmp"), "x").unwrap();
        assert!(t.rx.recv_timeout(Duration::from_millis(800)).is_err());
    }

    #[test]
    fn paused_dir_drops_events_until_resumed() {
        let t = start(150);
        t.w.pause_dir(t.dir.path());
        assert!(t.w.is_dir_paused(t.dir.path()));
        std::fs::write(t.dir.path().join("a.md"), "x").unwrap();
        assert!(t.rx.recv_timeout(Duration::from_millis(800)).is_err());
        t.w.resume_dir(t.dir.path());
        let p = t.dir.path().join("b.md");
        std::fs::write(&p, "y").unwrap();
        assert_eq!(t.rx.recv_timeout(WAIT).unwrap().modify, vec![p]);
    }

    #[test]
    fn flush_now_skips_the_debounce() {
        let t = start(60_000);
        let p = t.dir.path().join("a.md");
        std::fs::write(&p, "x").unwrap();
        t.queued.recv_timeout(WAIT).unwrap();
        t.w.flush_now();
        assert_eq!(t.rx.recv_timeout(Duration::from_secs(2)).unwrap().modify, vec![p]);
    }

    #[test]
    fn watch_dir_adds_a_second_root() {
        let t = start(150);
        let other = tempfile::tempdir().unwrap();
        t.w.watch_dir(other.path()).unwrap();
        let p = other.path().join("c.md");
        std::fs::write(&p, "x").unwrap();
        assert_eq!(t.rx.recv_timeout(WAIT).unwrap().modify, vec![p]);
    }
}
