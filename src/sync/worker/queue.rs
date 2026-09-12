use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Manages pending sync and delete paths with per-path debounce deadlines and capacity limits.
#[derive(Debug)]
pub(crate) struct DebounceQueue {
    pub(crate) pending_syncs: HashMap<PathBuf, Instant>,
    pub(crate) pending_deletes: HashMap<PathBuf, Instant>,
    pub(crate) sync_heap: BinaryHeap<Reverse<(Instant, PathBuf)>>,
    pub(crate) delete_heap: BinaryHeap<Reverse<(Instant, PathBuf)>>,
    pub(crate) max_capacity: usize,
}

impl DebounceQueue {
    /// Create a new debounce queue with given capacity.
    pub fn new(max_capacity: usize) -> Self {
        Self {
            pending_syncs: HashMap::new(),
            pending_deletes: HashMap::new(),
            sync_heap: BinaryHeap::new(),
            delete_heap: BinaryHeap::new(),
            max_capacity,
        }
    }

    /// Enqueue a path for sync with a debounce duration.
    /// If the path is already pending, its deadline is extended.
    /// Returns false if at capacity and the path was not already pending.
    pub fn enqueue_sync(&mut self, path: PathBuf, debounce: Duration) -> bool {
        let is_tracked =
            self.pending_syncs.contains_key(&path) || self.pending_deletes.contains_key(&path);
        if !is_tracked && self.pending_syncs.len() + self.pending_deletes.len() >= self.max_capacity
        {
            return false;
        }
        self.pending_deletes.remove(&path);
        let dl = Instant::now() + debounce;
        self.pending_syncs.insert(path.clone(), dl);
        self.sync_heap.push(Reverse((dl, path)));
        self.compact_heaps();
        true
    }

    /// Enqueue a path for deletion with a debounce duration.
    /// If the path is already pending, its deadline is extended.
    /// Returns false if at capacity and the path was not already pending.
    pub fn enqueue_delete(&mut self, path: PathBuf, debounce: Duration) -> bool {
        let is_tracked =
            self.pending_syncs.contains_key(&path) || self.pending_deletes.contains_key(&path);
        if !is_tracked && self.pending_syncs.len() + self.pending_deletes.len() >= self.max_capacity
        {
            return false;
        }
        self.pending_syncs.remove(&path);
        let dl = Instant::now() + debounce;
        self.pending_deletes.insert(path.clone(), dl);
        self.delete_heap.push(Reverse((dl, path)));
        self.compact_heaps();
        true
    }

    fn compact_single_heap(
        heap: &mut BinaryHeap<Reverse<(Instant, PathBuf)>>,
        active_items: &HashMap<PathBuf, Instant>,
    ) {
        if heap.len() > 64 && heap.len() > active_items.len() * 2 {
            let valid_entries: Vec<Reverse<(Instant, PathBuf)>> = heap
                .drain()
                .filter(|Reverse((deadline, path))| active_items.get(path) == Some(deadline))
                .collect();
            *heap = BinaryHeap::from(valid_entries);
        }
    }

    fn compact_heaps(&mut self) {
        Self::compact_single_heap(&mut self.sync_heap, &self.pending_syncs);
        Self::compact_single_heap(&mut self.delete_heap, &self.pending_deletes);
    }

    /// Drain and return all sync paths whose debounce deadlines are <= `now`.
    pub fn drain_ready_syncs(&mut self, now: Instant) -> Vec<PathBuf> {
        let mut ready = Vec::new();
        while let Some(Reverse((deadline, _path))) = self.sync_heap.peek() {
            if *deadline > now {
                break;
            }
            if let Some(Reverse((deadline, path))) = self.sync_heap.pop()
                && self.pending_syncs.get(&path) == Some(&deadline)
            {
                self.pending_syncs.remove(&path);
                ready.push(path);
            }
        }
        ready
    }

    /// Drain and return all delete paths whose debounce deadlines are <= `now`.
    pub fn drain_ready_deletes(&mut self, now: Instant) -> Vec<PathBuf> {
        let mut ready = Vec::new();
        while let Some(Reverse((deadline, _path))) = self.delete_heap.peek() {
            if *deadline > now {
                break;
            }
            if let Some(Reverse((deadline, path))) = self.delete_heap.pop()
                && self.pending_deletes.get(&path) == Some(&deadline)
            {
                self.pending_deletes.remove(&path);
                ready.push(path);
            }
        }
        ready
    }

    /// Re-enqueue a failed sync path for retry with a backoff delay.
    pub fn requeue_sync_retry(&mut self, path: PathBuf, delay: Duration) {
        let dl = Instant::now() + delay;
        self.pending_syncs.insert(path.clone(), dl);
        self.sync_heap.push(Reverse((dl, path)));
    }

    /// Re-enqueue a failed delete path for retry with a backoff delay.
    pub fn requeue_delete_retry(&mut self, path: PathBuf, delay: Duration) {
        let dl = Instant::now() + delay;
        self.pending_deletes.insert(path.clone(), dl);
        self.delete_heap.push(Reverse((dl, path)));
    }

    /// Calculate earliest deadline across all pending syncs and deletes.
    ///
    /// Uses min-heap top with lazy eviction of stale entries (whose deadline
    /// was updated or path was drained).
    pub fn earliest_deadline(&mut self) -> Option<Instant> {
        let sync_earliest = loop {
            match self.sync_heap.peek() {
                Some(Reverse((deadline, path))) => {
                    if self.pending_syncs.get(path) == Some(deadline) {
                        break Some(*deadline);
                    } else {
                        self.sync_heap.pop();
                    }
                }
                None => break None,
            }
        };
        let delete_earliest = loop {
            match self.delete_heap.peek() {
                Some(Reverse((deadline, path))) => {
                    if self.pending_deletes.get(path) == Some(deadline) {
                        break Some(*deadline);
                    } else {
                        self.delete_heap.pop();
                    }
                }
                None => break None,
            }
        };
        match (sync_earliest, delete_earliest) {
            (Some(s), Some(d)) => Some(std::cmp::min(s, d)),
            (Some(s), None) => Some(s),
            (None, Some(d)) => Some(d),
            (None, None) => None,
        }
    }

    /// Return true if both pending sync and delete queues are empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.pending_syncs.is_empty() && self.pending_deletes.is_empty()
    }

    /// Return count of pending syncs.
    #[allow(dead_code)]
    pub fn pending_sync_count(&self) -> usize {
        self.pending_syncs.len()
    }

    /// Return count of pending deletes.
    #[allow(dead_code)]
    pub fn pending_delete_count(&self) -> usize {
        self.pending_deletes.len()
    }

    /// Return total count of pending syncs and deletes.
    pub fn pending_count(&self) -> usize {
        self.pending_syncs.len() + self.pending_deletes.len()
    }

    /// Returns the total number of pending operations (syncs + deletes).
    #[inline]
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.pending_count()
    }
}
