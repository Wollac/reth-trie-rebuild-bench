//! A counting global allocator, for reporting the peak heap of a run.
//!
//! The database is memory-mapped, so a process's resident size says nothing about what the build
//! itself allocates. Counting at the allocator gives the heap alone: the builders' stacks, the
//! chunks waiting to be written, and whatever the cursors hold.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
};

/// Wraps the system allocator and tracks the current and peak number of live heap bytes.
///
/// ```ignore
/// #[global_allocator]
/// static GLOBAL: PeakAlloc = PeakAlloc::new();
/// ```
#[derive(Debug)]
pub struct PeakAlloc {
    current: AtomicUsize,
    peak: AtomicUsize,
}

impl PeakAlloc {
    /// Creates the allocator with zeroed counters.
    pub const fn new() -> Self {
        Self { current: AtomicUsize::new(0), peak: AtomicUsize::new(0) }
    }

    /// Live heap bytes right now.
    pub fn current(&self) -> usize {
        self.current.load(Ordering::Relaxed)
    }

    /// The highest number of live heap bytes seen since the last [`Self::reset_peak`].
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }

    /// Sets the peak to the current live size, so the next peak is that of the work that follows.
    pub fn reset_peak(&self) {
        self.peak.store(self.current(), Ordering::Relaxed);
    }

    fn add(&self, bytes: usize) {
        let now = self.current.fetch_add(bytes, Ordering::Relaxed) + bytes;
        self.peak.fetch_max(now, Ordering::Relaxed);
    }

    fn sub(&self, bytes: usize) {
        self.current.fetch_sub(bytes, Ordering::Relaxed);
    }
}

impl Default for PeakAlloc {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: every call is forwarded to the system allocator unchanged; the counters are only
// bookkeeping and never influence which memory is returned.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for PeakAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: same contract as the caller's.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            self.add(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: same contract as the caller's.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            self.add(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) };
        self.sub(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: same contract as the caller's.
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            self.sub(layout.size());
            self.add(new_size);
        }
        new
    }
}

/// Formats a byte count in binary units with two decimals, e.g. `1.50 GiB`.
pub fn human_bytes(bytes: usize) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.2} {}", UNITS[unit])
}
