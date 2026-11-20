// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jon Andrew
// Part of Penny, CuBit's web browser. Not derived from Servo's servoshell.

//! Thread census (CuBit only): which threads Penny has alive, by name.
//!
//! The kernel gives a process a bounded number of threads (WEB-001: slack.com
//! ran into it). The link wraps `pthread_create`, `pthread_exit`,
//! `pthread_setname_np` and `prctl` (`-Wl,--wrap`, build.rs), so every thread
//! started through the C library — Rust's std, GLib/GStreamer, C++ — is
//! counted from creation to exit and named by what it calls itself.
//!
//! Log lines (serial, no allocation on these paths):
//!   PENNY-THREADS peak=<live> name=<name> creator=<name>   each new peak
//!   PENNY-THREADS refused=<errno> live=<n> ...             the first refusal,
//!   PENNY-THREADS   <count> <name> by <creator>            then the census
//!   PENNY-THREADS site=<site> live=<n>                     per survey site,
//!                                                          then the census
use std::ffi::{c_char, c_int, c_ulong, c_void};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU8, AtomicUsize, Ordering};

type PthreadT = c_ulong;
type StartRoutine = extern "C" fn(*mut c_void) -> *mut c_void;

unsafe extern "C" {
    fn cubit_debug_write(data: *const u8, len: usize);
    fn __real_pthread_create(thread: *mut PthreadT, attr: *const c_void,
                             start: StartRoutine, arg: *mut c_void) -> c_int;
    fn __real_pthread_exit(value: *mut c_void) -> !;
    fn __real_pthread_setname_np(thread: PthreadT, name: *const c_char) -> c_int;
    fn __real_prctl(option: c_int, a2: c_ulong, a3: c_ulong, a4: c_ulong, a5: c_ulong) -> c_int;
    fn pthread_self() -> PthreadT;
}

/// Threads followed by name at once. The kernel's per-process limit is
/// below this; threads beyond it are counted, not named.
const TRACKED_THREADS: usize = 512;
/// Name bytes kept per thread (Linux keeps 15 plus the terminator).
const NAME_BYTES: usize = 32;
/// `prctl(PR_SET_NAME, name)`: GStreamer names its streaming threads so.
const PR_SET_NAME: c_int = 15;
/// The refusal the census reports (EAGAIN: no thread resources left).
const EAGAIN: c_int = 11;
/// Width of one log line's buffer.
const LINE_BYTES: usize = 160;

const FREE: u8 = 0;
const CLAIMED: u8 = 1;

struct Slot {
    state: AtomicU8,
    thread: AtomicUsize,
    start: AtomicPtr<c_void>,
    argument: AtomicPtr<c_void>,
    name: [AtomicU8; NAME_BYTES],
    creator: [AtomicU8; NAME_BYTES],
}

#[allow(clippy::declare_interior_mutable_const)]
const EMPTY_SLOT: Slot = Slot {
    state: AtomicU8::new(FREE),
    thread: AtomicUsize::new(0),
    start: AtomicPtr::new(std::ptr::null_mut()),
    argument: AtomicPtr::new(std::ptr::null_mut()),
    name: [const { AtomicU8::new(0) }; NAME_BYTES],
    creator: [const { AtomicU8::new(0) }; NAME_BYTES],
};

static SLOTS: [Slot; TRACKED_THREADS] = [EMPTY_SLOT; TRACKED_THREADS];
/// Threads alive now that the census created (named or not); the main
/// thread is not among them.
static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicI64 = AtomicI64::new(0);
static UNTRACKED: AtomicI64 = AtomicI64::new(0);
static REFUSAL_REPORTED: AtomicBool = AtomicBool::new(false);

/// No slot: the main thread, or a thread created when the table was full.
const NO_SLOT: usize = usize::MAX;
thread_local! {
    static CURRENT: std::cell::Cell<usize> = const { std::cell::Cell::new(NO_SLOT) };
}

struct Line { bytes: [u8; LINE_BYTES], length: usize }
impl Line {
    fn new() -> Self { Line { bytes: [0; LINE_BYTES], length: 0 } }
    fn text(&mut self, text: &[u8]) -> &mut Self {
        for &byte in text {
            if self.length < LINE_BYTES - 1 { self.bytes[self.length] = byte; self.length += 1; }
        }
        self
    }
    fn number(&mut self, value: i64) -> &mut Self {
        let mut digits = [0u8; 20];
        let mut count = 0;
        let mut rest = value.unsigned_abs();
        loop {
            digits[count] = b'0' + (rest % 10) as u8;
            count += 1;
            rest /= 10;
            if rest == 0 { break; }
        }
        if value < 0 { self.text(b"-"); }
        for index in (0..count).rev() { self.text(&digits[index..index + 1]); }
        self
    }
    fn name(&mut self, name: &[AtomicU8; NAME_BYTES]) -> &mut Self {
        let mut any = false;
        for byte in name.iter().map(|b| b.load(Ordering::Relaxed)).take_while(|&b| b != 0) {
            let shown = if byte.is_ascii_graphic() { byte } else { b'_' };
            self.text(&[shown]);
            any = true;
        }
        if !any { self.text(b"(unnamed)"); }
        self
    }
    fn write(&mut self) {
        self.text(b"\n");
        if self.length == LINE_BYTES - 1 { self.bytes[LINE_BYTES - 1] = b'\n'; self.length = LINE_BYTES; }
        unsafe { cubit_debug_write(self.bytes.as_ptr(), self.length) };
    }
}

fn store_name(target: &[AtomicU8; NAME_BYTES], name: *const c_char) {
    let mut ended = name.is_null();
    for (index, byte) in target.iter().enumerate() {
        let value = if ended || index == NAME_BYTES - 1 { 0 } else { unsafe { *name.add(index) as u8 } };
        if value == 0 { ended = true; }
        byte.store(value, Ordering::Relaxed);
    }
}

fn copy_name(target: &[AtomicU8; NAME_BYTES], source: Option<&[AtomicU8; NAME_BYTES]>, fallback: &[u8]) {
    for (index, byte) in target.iter().enumerate() {
        let value = match source {
            Some(name) => name[index].load(Ordering::Relaxed),
            None => fallback.get(index).copied().unwrap_or(0),
        };
        byte.store(if index == NAME_BYTES - 1 { 0 } else { value }, Ordering::Relaxed);
    }
}

fn current_name() -> Option<&'static [AtomicU8; NAME_BYTES]> {
    let slot = CURRENT.try_with(|c| c.get()).unwrap_or(NO_SLOT);
    (slot != NO_SLOT).then(|| &SLOTS[slot].name)
}

fn claim() -> usize {
    for (index, slot) in SLOTS.iter().enumerate() {
        if slot.state.compare_exchange(FREE, CLAIMED, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            return index;
        }
    }
    NO_SLOT
}

fn release(index: usize) {
    SLOTS[index].thread.store(0, Ordering::Relaxed);
    SLOTS[index].state.store(FREE, Ordering::Release);
}

extern "C" fn census_start(argument: *mut c_void) -> *mut c_void {
    let index = argument as usize;
    let slot = &SLOTS[index];
    slot.thread.store(unsafe { pthread_self() } as usize, Ordering::Relaxed);
    let _ = CURRENT.try_with(|c| c.set(index));
    let start: StartRoutine = unsafe { std::mem::transmute(slot.start.load(Ordering::Acquire)) };
    let result = start(slot.argument.load(Ordering::Acquire));
    exited();
    result
}

/// The calling thread is about to end.
fn exited() {
    let index = CURRENT.try_with(|c| c.replace(NO_SLOT)).unwrap_or(NO_SLOT);
    if index != NO_SLOT {
        release(index);
        LIVE.fetch_sub(1, Ordering::AcqRel);
    }
}

fn note_peak(live: i64, index: usize) {
    let mut peak = PEAK.load(Ordering::Relaxed);
    while live > peak {
        match PEAK.compare_exchange_weak(peak, live, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => {
                let mut line = Line::new();
                line.text(b"PENNY-THREADS peak=").number(live).text(b" creator=");
                if index != NO_SLOT { line.name(&SLOTS[index].creator); } else { line.text(b"(untracked)"); }
                line.write();
                return;
            }
            Err(now) => peak = now,
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wrap_pthread_create(thread: *mut PthreadT, attr: *const c_void,
                                              start: StartRoutine, argument: *mut c_void) -> c_int {
    let index = claim();
    let live = LIVE.fetch_add(1, Ordering::AcqRel) + 1;
    let result = if index == NO_SLOT {
        UNTRACKED.fetch_add(1, Ordering::Relaxed);
        unsafe { __real_pthread_create(thread, attr, start, argument) }
    } else {
        let slot = &SLOTS[index];
        slot.start.store(start as *mut c_void, Ordering::Release);
        slot.argument.store(argument, Ordering::Release);
        copy_name(&slot.name, None, b"");
        copy_name(&slot.creator, current_name(), b"main");
        unsafe { __real_pthread_create(thread, attr, census_start, index as *mut c_void) }
    };
    if result != 0 {
        LIVE.fetch_sub(1, Ordering::AcqRel);
        if index != NO_SLOT { release(index); } else { UNTRACKED.fetch_sub(1, Ordering::Relaxed); }
        if !REFUSAL_REPORTED.swap(true, Ordering::AcqRel) {
            let mut line = Line::new();
            line.text(b"PENNY-THREADS refused=").number(result as i64)
                .text(if result == EAGAIN { b" (EAGAIN)" } else { b"" })
                .text(b" live=").number(live - 1).text(b" peak=").number(PEAK.load(Ordering::Relaxed))
                .text(b" untracked=").number(UNTRACKED.load(Ordering::Relaxed)).text(b" creator=");
            match current_name() { Some(name) => line.name(name), None => line.text(b"main") };
            line.write();
            census();
        }
    } else {
        note_peak(live, index);
    }
    result
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wrap_pthread_exit(value: *mut c_void) -> ! {
    exited();
    unsafe { __real_pthread_exit(value) }
}

fn rename(thread: PthreadT, name: *const c_char) {
    let own = unsafe { pthread_self() } == thread;
    let index = if own {
        CURRENT.try_with(|c| c.get()).unwrap_or(NO_SLOT)
    } else {
        SLOTS.iter().position(|slot| slot.state.load(Ordering::Acquire) == CLAIMED
            && slot.thread.load(Ordering::Relaxed) == thread as usize).unwrap_or(NO_SLOT)
    };
    if index != NO_SLOT { store_name(&SLOTS[index].name, name); }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wrap_pthread_setname_np(thread: PthreadT, name: *const c_char) -> c_int {
    rename(thread, name);
    unsafe { __real_pthread_setname_np(thread, name) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wrap_prctl(option: c_int, a2: c_ulong, a3: c_ulong, a4: c_ulong, a5: c_ulong) -> c_int {
    if option == PR_SET_NAME { rename(unsafe { pthread_self() }, a2 as *const c_char); }
    unsafe { __real_prctl(option, a2, a3, a4, a5) }
}

fn same(a: &[AtomicU8; NAME_BYTES], b: &[AtomicU8; NAME_BYTES]) -> bool {
    a.iter().zip(b.iter()).all(|(x, y)| x.load(Ordering::Relaxed) == y.load(Ordering::Relaxed))
}

/// Live threads grouped by (name with trailing digits kept, creator): one
/// line per group, first appearance order. No allocation.
fn census() {
    let live = |slot: &Slot| slot.state.load(Ordering::Acquire) == CLAIMED;
    for (index, slot) in SLOTS.iter().enumerate() {
        if !live(slot) { continue; }
        let first = SLOTS[..index].iter().all(|earlier| !live(earlier)
            || !same(&earlier.name, &slot.name) || !same(&earlier.creator, &slot.creator));
        if !first { continue; }
        let count = SLOTS[index..].iter().filter(|later| live(later)
            && same(&later.name, &slot.name) && same(&later.creator, &slot.creator)).count();
        let mut line = Line::new();
        line.text(b"PENNY-THREADS   ").number(count as i64).text(b" ").name(&slot.name)
            .text(b" by ").name(&slot.creator);
        line.write();
    }
}

/// The census at the start of a survey site.
pub fn report(site: &str) {
    let mut line = Line::new();
    line.text(b"PENNY-THREADS site=").text(site.as_bytes()).text(b" live=").number(LIVE.load(Ordering::Relaxed))
        .text(b" peak=").number(PEAK.load(Ordering::Relaxed))
        .text(b" untracked=").number(UNTRACKED.load(Ordering::Relaxed));
    line.write();
    census();
}
