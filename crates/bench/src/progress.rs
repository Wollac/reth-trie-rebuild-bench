//! Progress lines for a long build, from the entry counts the sink receives.

use alloy_primitives::B256;
use reth_trie_common::{BranchNodeCompact, Nibbles};
use reth_trie_parallel::partitioned_root::TrieSink;
use std::{
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

/// A [`TrieSink`] that passes nodes on to an inner sink and logs a progress line at most once per
/// interval: the share of the expected entries walked so far and the time remaining at the
/// average rate.
pub struct ProgressSink<S, F> {
    inner: S,
    /// The expected number of hashed entries, from the table stats.
    total: u64,
    walked: AtomicU64,
    started: Instant,
    interval: Duration,
    last_log: Mutex<Instant>,
    log: F,
}

impl<S: TrieSink, F: Fn(&str) + Sync> ProgressSink<S, F> {
    /// Wraps `inner`, expecting `total` hashed entries and logging at most every `interval`.
    pub fn new(inner: S, total: u64, interval: Duration, log: F) -> Self {
        let now = Instant::now();
        Self {
            inner,
            total,
            walked: AtomicU64::new(0),
            started: now,
            interval,
            last_log: Mutex::new(now),
            log,
        }
    }

    /// The inner sink.
    pub const fn inner(&self) -> &S {
        &self.inner
    }

    /// Consumes the wrapper and returns the inner sink.
    pub fn into_inner(self) -> S {
        self.inner
    }

    /// The number of hashed entries walked so far.
    pub fn walked(&self) -> u64 {
        self.walked.load(Ordering::Relaxed)
    }
}

impl<S: TrieSink, F: Fn(&str) + Sync> TrieSink for ProgressSink<S, F> {
    fn on_branch_node(&self, hashed_address: Option<B256>, path: Nibbles, node: BranchNodeCompact) {
        self.inner.on_branch_node(hashed_address, path, node)
    }

    fn on_progress(&self, entries: u64) {
        if entries == 0 {
            return;
        }
        let walked = self.walked.fetch_add(entries, Ordering::Relaxed) + entries;
        // Batches arrive rarely enough that the lock is never a bottleneck. It is held while
        // logging, so lines do not interleave.
        let mut last_log = self.last_log.lock().expect("lock is not poisoned");
        if last_log.elapsed() < self.interval {
            return;
        }
        *last_log = Instant::now();

        let elapsed = self.started.elapsed();
        let remaining = self.total.saturating_sub(walked);
        let eta = elapsed.mul_f64(remaining as f64 / walked as f64);
        (self.log)(&format!(
            "progress {:.2}%: {walked} of {} entries walked, {elapsed:.0?} elapsed, {eta:.0?} remaining",
            100.0 * walked as f64 / self.total.max(1) as f64,
            self.total,
        ));
    }
}

impl<S: fmt::Debug, F> fmt::Debug for ProgressSink<S, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProgressSink")
            .field("inner", &self.inner)
            .field("total", &self.total)
            .field("walked", &self.walked)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}
