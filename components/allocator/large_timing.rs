/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Opt-in timing of large allocations (WEB-001: the display-list cost on
//! CuBit). Identical on CuBit and hosted Linux: the platform allocator is
//! wrapped, and while timing is enabled every allocation, reallocation or
//! free of at least `LARGE_BYTES` is counted and timed. Disabled (the
//! default), a large request costs one relaxed atomic load more; a small one
//! only the size comparison. The embedder enables it and reads the counters
//! through the C-ABI functions below (cubitshell's bench, flag alloc-timing).

use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// Large: CuAlloc gives each block from this size its own reservation.
pub const LARGE_BYTES: usize = 1 << 20;

/// The counters, in the order `servo_large_allocations_read` returns them.
#[derive(Clone, Copy)]
#[repr(usize)]
enum Counter {
    Allocations,
    AllocationNanos,
    Frees,
    FreeNanos,
    Reallocations,
    ReallocationNanos,
    AllocatedBytes,
    LongestNanos,
}
const COUNTERS: usize = Counter::LongestNanos as usize + 1;

static ENABLED: AtomicBool = AtomicBool::new(false);
static VALUES: [AtomicU64; COUNTERS] = [const { AtomicU64::new(0) }; COUNTERS];

fn add(counter: Counter, value: u64) {
    VALUES[counter as usize].fetch_add(value, Ordering::Relaxed);
}

fn timed<T>(count: Counter, nanos: Counter, bytes: usize, operation: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = operation();
    let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    add(count, 1);
    add(nanos, elapsed);
    add(Counter::AllocatedBytes, bytes as u64);
    VALUES[Counter::LongestNanos as usize].fetch_max(elapsed, Ordering::Relaxed);
    result
}

fn watched(size: usize) -> bool {
    size >= LARGE_BYTES && ENABLED.load(Ordering::Relaxed)
}

/// The platform allocator with large-allocation timing.
pub struct Timed<A>(pub A);

unsafe impl<A: GlobalAlloc> GlobalAlloc for Timed<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !watched(layout.size()) {
            return unsafe { self.0.alloc(layout) };
        }
        timed(Counter::Allocations, Counter::AllocationNanos, layout.size(),
              || unsafe { self.0.alloc(layout) })
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if !watched(layout.size()) {
            return unsafe { self.0.alloc_zeroed(layout) };
        }
        timed(Counter::Allocations, Counter::AllocationNanos, layout.size(),
              || unsafe { self.0.alloc_zeroed(layout) })
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if !watched(layout.size()) {
            return unsafe { self.0.dealloc(pointer, layout) };
        }
        timed(Counter::Frees, Counter::FreeNanos, 0, || unsafe { self.0.dealloc(pointer, layout) })
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if !watched(layout.size().max(new_size)) {
            return unsafe { self.0.realloc(pointer, layout, new_size) };
        }
        timed(Counter::Reallocations, Counter::ReallocationNanos, new_size,
              || unsafe { self.0.realloc(pointer, layout, new_size) })
    }
}

/// Starts or stops timing large allocations.
#[unsafe(no_mangle)]
pub extern "C" fn servo_large_allocations_enable(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Copies up to `capacity` counters into `out` (see `Counter` for the
/// order) and returns how many there are.
///
/// # Safety
/// `out` must be valid for writing `capacity` values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn servo_large_allocations_read(out: *mut u64, capacity: usize) -> usize {
    for (index, value) in VALUES.iter().enumerate().take(capacity) {
        unsafe { out.add(index).write(value.load(Ordering::Relaxed)) };
    }
    COUNTERS
}
