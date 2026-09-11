//! Requested Rust allocation payloads, not allocator arenas or OS working set.
//! Keep all hooks allocation-free and delegate the original layouts unchanged.
use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static REALLOCS: AtomicU64 = AtomicU64::new(0);
static DEALLOCS: AtomicU64 = AtomicU64::new(0);
static REQUESTED: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);
static FAILED: AtomicU64 = AtomicU64::new(0);

pub struct Counting<A>(pub A);

fn increase(bytes: usize) {
    let live = LIVE.fetch_add(bytes as u64, Relaxed) + bytes as u64;
    PEAK.fetch_max(live, Relaxed);
}

// Safety: the wrapper never changes pointers, layouts, alignment or ownership.
// It performs only non-allocating atomic bookkeeping around the inner allocator.
unsafe impl<A: GlobalAlloc> GlobalAlloc for Counting<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { self.0.alloc(layout) };
        if pointer.is_null() {
            FAILED.fetch_add(1, Relaxed);
        } else {
            ALLOCS.fetch_add(1, Relaxed);
            REQUESTED.fetch_add(layout.size() as u64, Relaxed);
            increase(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { self.0.alloc_zeroed(layout) };
        if pointer.is_null() {
            FAILED.fetch_add(1, Relaxed);
        } else {
            ALLOCS.fetch_add(1, Relaxed);
            REQUESTED.fetch_add(layout.size() as u64, Relaxed);
            increase(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        DEALLOCS.fetch_add(1, Relaxed);
        LIVE.fetch_sub(layout.size() as u64, Relaxed);
        unsafe { self.0.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let result = unsafe { self.0.realloc(pointer, layout, new_size) };
        if result.is_null() {
            FAILED.fetch_add(1, Relaxed);
        } else {
            REALLOCS.fetch_add(1, Relaxed);
            // Count the whole new request, not just its growth. In-place
            // reallocations therefore still contribute requested bytes.
            REQUESTED.fetch_add(new_size as u64, Relaxed);
            if new_size >= layout.size() {
                increase(new_size - layout.size());
            } else {
                LIVE.fetch_sub((layout.size() - new_size) as u64, Relaxed);
            }
        }
        result
    }
}

#[derive(Clone, Copy)]
pub struct Snapshot {
    allocs: u64,
    reallocs: u64,
    deallocs: u64,
    requested: u64,
    live: u64,
    failed: u64,
}

pub fn begin() -> Snapshot {
    let live = LIVE.load(Relaxed);
    PEAK.store(live, Relaxed);
    Snapshot {
        allocs: ALLOCS.load(Relaxed),
        reallocs: REALLOCS.load(Relaxed),
        deallocs: DEALLOCS.load(Relaxed),
        requested: REQUESTED.load(Relaxed),
        live,
        failed: FAILED.load(Relaxed),
    }
}

pub fn finish(before: Snapshot) -> serde_json::Value {
    // Capture every counter before JSON construction can allocate.
    let allocs = ALLOCS.load(Relaxed) - before.allocs;
    let reallocs = REALLOCS.load(Relaxed) - before.reallocs;
    let deallocs = DEALLOCS.load(Relaxed) - before.deallocs;
    let requested = REQUESTED.load(Relaxed) - before.requested;
    let failed = FAILED.load(Relaxed) - before.failed;
    let live = LIVE.load(Relaxed);
    let peak = PEAK.load(Relaxed);
    serde_json::json!({"allocation_calls": allocs, "reallocation_calls": reallocs,
        "deallocation_calls": deallocs, "requested_bytes": requested, "failed_allocations": failed,
        "live_requested_bytes_before": before.live, "live_requested_bytes_after": live,
        "peak_live_requested_bytes": peak, "peak_growth_requested_bytes": peak.saturating_sub(before.live)})
}
