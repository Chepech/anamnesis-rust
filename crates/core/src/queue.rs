//! Debounced, deduplicated queue between raw file events and the indexer.
//! Many events for one path collapse to one op; the last event decides modify vs delete.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Modify,
    Delete,
}

#[derive(Debug, Default, PartialEq)]
pub struct Batch {
    pub modify: Vec<PathBuf>,
    pub delete: Vec<PathBuf>,
}

impl Batch {
    pub fn is_empty(&self) -> bool {
        self.modify.is_empty() && self.delete.is_empty()
    }
}

#[derive(Default)]
pub struct PendingQueue {
    ops: HashMap<PathBuf, Op>,
    deadline: Option<Instant>,
}

impl PendingQueue {
    /// Records an op and pushes the flush deadline to `now + debounce` (trailing debounce).
    pub fn push(&mut self, path: PathBuf, op: Op, now: Instant, debounce: Duration) {
        self.ops.insert(path, op);
        self.deadline = Some(now + debounce);
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// The pending batch if the deadline has passed.
    pub fn take_due(&mut self, now: Instant) -> Option<Batch> {
        (self.deadline? <= now).then(|| self.take_all())
    }

    /// Everything pending, deadline ignored ("Index now").
    pub fn take_all(&mut self) -> Batch {
        self.deadline = None;
        let mut b = Batch::default();
        for (p, op) in self.ops.drain() {
            match op {
                Op::Modify => b.modify.push(p),
                Op::Delete => b.delete.push(p),
            }
        }
        b.modify.sort();
        b.delete.sort();
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: Duration = Duration::from_millis(100);

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn empty_queue_is_never_due() {
        let mut q = PendingQueue::default();
        assert!(q.take_due(Instant::now() + D * 10).is_none());
        assert!(q.take_all().is_empty());
    }

    #[test]
    fn batch_is_due_only_after_debounce() {
        let t0 = Instant::now();
        let mut q = PendingQueue::default();
        q.push(p("/a.md"), Op::Modify, t0, D);
        assert!(q.take_due(t0 + D / 2).is_none());
        let b = q.take_due(t0 + D).unwrap();
        assert_eq!(b.modify, vec![p("/a.md")]);
        assert!(q.is_empty() && q.deadline().is_none());
    }

    #[test]
    fn every_event_resets_the_deadline() {
        let t0 = Instant::now();
        let mut q = PendingQueue::default();
        q.push(p("/a.md"), Op::Modify, t0, D);
        q.push(p("/b.md"), Op::Modify, t0 + D * 9 / 10, D);
        assert!(
            q.take_due(t0 + D).is_none(),
            "second event extended the window"
        );
        assert_eq!(q.take_due(t0 + D * 2).unwrap().modify.len(), 2);
    }

    #[test]
    fn repeated_events_for_one_path_dedup() {
        let t0 = Instant::now();
        let mut q = PendingQueue::default();
        for _ in 0..50 {
            q.push(p("/a.md"), Op::Modify, t0, D);
        }
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn last_op_wins_between_modify_and_delete() {
        let t0 = Instant::now();
        let mut q = PendingQueue::default();
        q.push(p("/gone.md"), Op::Modify, t0, D);
        q.push(p("/gone.md"), Op::Delete, t0, D);
        q.push(p("/back.md"), Op::Delete, t0, D);
        q.push(p("/back.md"), Op::Modify, t0, D);
        let b = q.take_all();
        assert_eq!(b.delete, vec![p("/gone.md")]);
        assert_eq!(b.modify, vec![p("/back.md")]);
    }

    #[test]
    fn take_all_ignores_deadline_and_sorts_paths() {
        let t0 = Instant::now();
        let mut q = PendingQueue::default();
        q.push(p("/c.md"), Op::Modify, t0, D);
        q.push(p("/a.md"), Op::Modify, t0, D);
        q.push(p("/b.md"), Op::Delete, t0, D);
        let b = q.take_all();
        assert_eq!(b.modify, vec![p("/a.md"), p("/c.md")]);
        assert_eq!(b.delete, vec![p("/b.md")]);
        assert!(q.deadline().is_none());
    }
}
