//! Allocation-event attribution, never allocation ownership or retained heap.
//! Hooks use constant-initialized, destructor-free TLS only: no environment,
//! RefCell, locks, heap allocation, formatting or normal telemetry calls.
use serde::Serialize;
use std::{
    alloc::{GlobalAlloc, Layout},
    cell::Cell,
    marker::PhantomData,
    rc::Rc,
};

macro_rules! counter_fields {
    ($($field:ident),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
        pub struct Counts {
            $(pub $field: u64,)+
            /// Saturation or an invalid nested subtraction makes this partial.
            pub incomplete: bool,
        }
        impl Counts {
            pub fn difference(self, before: Self) -> Self {
                let mut incomplete = self.incomplete || before.incomplete;
                Self {
                    $($field: self.$field.checked_sub(before.$field).unwrap_or_else(|| {
                        incomplete = true;
                        0
                    }),)+
                    incomplete,
                }
            }
            pub fn add(&mut self, other: Self) {
                self.incomplete |= other.incomplete;
                $(self.$field = self.$field.checked_add(other.$field).unwrap_or_else(|| {
                    self.incomplete = true;
                    u64::MAX
                });)+
            }
        }
        struct Counters {
            $($field: Cell<u64>,)+
            incomplete: Cell<bool>,
            suppressed: Cell<bool>,
            recording: Cell<bool>,
        }
        impl Counters {
            const fn new() -> Self {
                Self { $($field: Cell::new(0),)+ incomplete: Cell::new(false),
                    suppressed: Cell::new(false), recording: Cell::new(false) }
            }
            fn snapshot(&self) -> Counts {
                Counts { $($field: self.$field.get(),)+ incomplete: self.incomplete.get() }
            }
            fn add(&self, field: &Cell<u64>, amount: u64) {
                field.set(field.get().checked_add(amount).unwrap_or_else(|| {
                    self.incomplete.set(true);
                    u64::MAX
                }));
            }
        }
    };
}
counter_fields! {
    allocation_calls, reallocation_calls, deallocation_calls,
    requested_bytes, deallocated_bytes, reallocated_old_bytes, failed_allocations,
}

thread_local! { static COUNTERS: Counters = const { Counters::new() }; }

#[inline]
pub fn snapshot() -> Counts {
    COUNTERS.try_with(Counters::snapshot).unwrap_or(Counts {
        incomplete: true,
        ..Counts::default()
    })
}

/// Exclude profiler bookkeeping, census, aggregation and export from application
/// counts. This guard is non-Send and nestable; restoration also runs on unwind.
pub struct Suppress(Option<bool>, PhantomData<Rc<()>>);
impl Suppress {
    #[inline]
    pub fn new() -> Self {
        Self(
            COUNTERS
                .try_with(|counters| counters.suppressed.replace(true))
                .ok(),
            PhantomData,
        )
    }
}
impl Drop for Suppress {
    #[inline]
    fn drop(&mut self) {
        if let Some(previous) = self.0 {
            let _ = COUNTERS.try_with(|counters| counters.suppressed.set(previous));
        }
    }
}

#[inline]
fn record(update: impl FnOnce(&Counters)) {
    let _ = COUNTERS.try_with(|counters| {
        if counters.suppressed.get() {
            return;
        }
        if counters.recording.replace(true) {
            counters.incomplete.set(true);
            return;
        }
        // Every update below is bounded integer/Cell work and cannot allocate
        // or unwind. A defensive reentry reports incomplete, never recurses.
        update(counters);
        counters.recording.set(false);
    });
}

#[inline]
pub fn allocated(size: usize) {
    record(|c| {
        c.add(&c.allocation_calls, 1);
        c.add(&c.requested_bytes, size as u64);
    });
}
#[inline]
pub fn reallocated(old_size: usize, new_size: usize) {
    record(|c| {
        c.add(&c.reallocation_calls, 1);
        c.add(&c.requested_bytes, new_size as u64);
        c.add(&c.reallocated_old_bytes, old_size as u64);
    });
}
#[inline]
pub fn deallocated(size: usize) {
    record(|c| {
        c.add(&c.deallocation_calls, 1);
        c.add(&c.deallocated_bytes, size as u64);
    });
}
#[inline]
pub fn failed() {
    record(|c| c.add(&c.failed_allocations, 1));
}

/// Optional wrapper; the executable retains the allocator choice. The ast
/// library never installs a production global allocator.
pub struct Tracing<A>(pub A);
// Safety: delegate every layout, alignment, pointer and ownership unchanged.
// The post-call hooks cannot allocate, take a lock, panic or call user code.
unsafe impl<A: GlobalAlloc> GlobalAlloc for Tracing<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { self.0.alloc(layout) };
        if pointer.is_null() {
            failed();
        } else {
            allocated(layout.size());
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { self.0.alloc_zeroed(layout) };
        if pointer.is_null() {
            failed();
        } else {
            allocated(layout.size());
        }
        pointer
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let result = unsafe { self.0.realloc(pointer, layout, new_size) };
        if result.is_null() {
            failed();
        } else {
            reallocated(layout.size(), new_size);
        }
        result
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { self.0.dealloc(pointer, layout) };
        deallocated(layout.size());
    }
}

#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: Tracing<std::alloc::System> = Tracing(std::alloc::System);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_allocator_events_count_zeroed_realloc_and_release_exactly() {
        let before = snapshot();
        unsafe {
            let layout = Layout::from_size_align_unchecked(16, 8);
            let pointer = std::alloc::alloc_zeroed(layout);
            assert!(!pointer.is_null());
            let pointer = std::alloc::realloc(pointer, layout, 40);
            assert!(!pointer.is_null());
            std::alloc::dealloc(pointer, Layout::from_size_align_unchecked(40, 8));
        }
        assert_eq!(
            snapshot().difference(before),
            Counts {
                allocation_calls: 1,
                reallocation_calls: 1,
                deallocation_calls: 1,
                requested_bytes: 56,
                deallocated_bytes: 40,
                reallocated_old_bytes: 16,
                ..Counts::default()
            }
        );
    }

    #[test]
    fn failed_reallocation_keeps_old_storage_and_counts_only_failure() {
        struct RejectReallocation;
        unsafe impl GlobalAlloc for RejectReallocation {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
                unsafe { std::alloc::System.alloc(layout) }
            }
            unsafe fn realloc(&self, _: *mut u8, _: Layout, _: usize) -> *mut u8 {
                std::ptr::null_mut()
            }
            unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
                unsafe { std::alloc::System.dealloc(pointer, layout) }
            }
        }
        let allocator = Tracing(RejectReallocation);
        let layout = Layout::from_size_align(8, 8).unwrap();
        let before = snapshot();
        unsafe {
            let pointer = allocator.alloc(layout);
            assert!(!pointer.is_null());
            pointer.write(37);
            assert!(allocator.realloc(pointer, layout, 16).is_null());
            assert_eq!(pointer.read(), 37);
            allocator.dealloc(pointer, layout);
        }
        assert_eq!(
            snapshot().difference(before),
            Counts {
                allocation_calls: 1,
                deallocation_calls: 1,
                requested_bytes: 8,
                deallocated_bytes: 8,
                failed_allocations: 1,
                ..Counts::default()
            }
        );
    }

    #[test]
    fn suppression_is_nested_and_restored_after_unwind() {
        let before = snapshot();
        {
            let _outer = Suppress::new();
            let _ = std::panic::catch_unwind(|| {
                let _inner = Suppress::new();
                let values = std::hint::black_box(vec![0u8; 127]);
                drop(values);
                panic!("suppressed allocation probe");
            });
            assert_eq!(snapshot(), before);
        }
        allocated(9);
        assert_eq!(
            snapshot().difference(before),
            Counts {
                allocation_calls: 1,
                requested_bytes: 9,
                ..Counts::default()
            }
        );
    }

    #[test]
    fn saturation_and_bad_subtraction_are_explicitly_partial() {
        let mut counts = Counts {
            allocation_calls: u64::MAX,
            ..Counts::default()
        };
        counts.add(Counts {
            allocation_calls: 1,
            ..Counts::default()
        });
        assert_eq!(counts.allocation_calls, u64::MAX);
        assert!(counts.incomplete);
        let difference = Counts::default().difference(Counts {
            requested_bytes: 1,
            ..Counts::default()
        });
        assert_eq!(difference.requested_bytes, 0);
        assert!(difference.incomplete);
    }

    #[test]
    fn hook_does_not_borrow_telemetry_state_or_recurse() {
        let before = snapshot();
        super::super::STATE.with(|state| {
            let _borrow = state.borrow_mut();
            allocated(3);
        });
        assert_eq!(snapshot().difference(before).allocation_calls, 1);
        // Exercise the defensive recursion boundary without making an actual
        // global allocator call while its hook is already active.
        COUNTERS.with(|c| {
            c.recording.set(true);
            allocated(99);
            c.recording.set(false);
            assert!(c.incomplete.get());
            c.incomplete.set(false);
        });
        assert_eq!(snapshot().difference(before).requested_bytes, 3);
    }
}
