/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The WEB-001 performance breakdown (`bench` flag): the same scripted
//! measurement on CuBit and on hosted Linux. After a page has loaded and
//! settled, it types keys (a page script echoes them: script, style, layout,
//! display list, WebRender, SWGL) and turns the wheel (compositor scrolling),
//! timing each input to its painted and presented frame. Samples are kept in
//! memory and printed after the run, so reporting does not delay frames.
//! Servo's time profile is printed after shutdown.

use std::time::{Duration, Instant};

use servo::{
    Code, DevicePoint, InputEvent, Key, KeyState, KeyboardEvent, Location, Modifiers,
    RenderingContext, WebView, WheelDelta, WheelEvent, WheelMode,
};

use crate::{Delegate, SwglContext, flag_path, say, spin_servo};

/// Inputs per page: enough for a median and spread, short enough to stay
/// within the article's length when scrolling.
const KEY_STEPS: usize = 20;
const SCROLL_STEPS: usize = 30;
/// An input with no new frame by then counts as a timeout.
const STEP_DEADLINE: Duration = Duration::from_secs(3);
/// Follow-up frames (key release, scroll events) are painted, not measured,
/// until none has arrived for this long.
const STEP_QUIET: Duration = Duration::from_millis(50);
/// The longest the loop sleeps between event-loop turns; Servo's waker
/// unparks it as soon as there is work.
pub const PARK: Duration = Duration::from_millis(1);
/// One wheel notch toward the page's end, as the interactive loop converts
/// it. Servo scrolls by the inverse of the wheel delta: negative is down.
const WHEEL_NOTCH_DOWN_PIXELS: f64 = -76.0;
/// The key the pages' echo script appends.
const KEY: &str = "a";
/// How long a page may take to reach its wait-title marker.
const TITLE_DEADLINE: Duration = Duration::from_secs(600);

// Servo's allocator wrapper (components/allocator/large_timing.rs).
unsafe extern "C" {
    fn servo_large_allocations_enable(enabled: bool);
    fn servo_large_allocations_read(out: *mut u64, capacity: usize) -> usize;
}

/// The wrapper's counters, in its order.
const LARGE_COUNTERS: usize = 8;
const LARGE_ALLOCATIONS: usize = 0;
const LARGE_ALLOCATION_NANOS: usize = 1;
const LARGE_FREES: usize = 2;
const LARGE_FREE_NANOS: usize = 3;
const LARGE_REALLOCATIONS: usize = 4;
const LARGE_REALLOCATION_NANOS: usize = 5;
const NANOS_PER_MICRO: u64 = 1000;

fn large_counters() -> [u64; LARGE_COUNTERS] {
    let mut values = [0; LARGE_COUNTERS];
    // Safety: the buffer holds LARGE_COUNTERS values.
    unsafe { servo_large_allocations_read(values.as_mut_ptr(), values.len()) };
    values
}

#[derive(Clone, Copy)]
pub struct Sample {
    /// Input sent to Servo's new-frame notification.
    ready: Duration,
    /// WebView::paint: WebRender update and SWGL draw.
    paint: Duration,
    /// RenderingContext::present: the frame copy to the window.
    present: Duration,
    /// Large allocations from input until the step settled (alloc-timing flag;
    /// zero otherwise): counter deltas in the wrapper's order.
    large: [u64; LARGE_COUNTERS],
}

impl Sample {
    fn report(&self, kind: &str, step: usize) {
        say(&format!(
            "PENNY-BENCH: kind={kind} step={step} ready_us={} paint_us={} present_us={} total_us={}",
            self.ready.as_micros(),
            self.paint.as_micros(),
            self.present.as_micros(),
            (self.ready + self.paint + self.present).as_micros()
        ));
        let l = &self.large;
        say(&format!(
            "PENNY-ALLOC: kind={kind} step={step} allocations={} allocate_us={} frees={} free_us={} reallocations={} reallocate_us={}",
            l[LARGE_ALLOCATIONS], l[LARGE_ALLOCATION_NANOS] / NANOS_PER_MICRO,
            l[LARGE_FREES], l[LARGE_FREE_NANOS] / NANOS_PER_MICRO,
            l[LARGE_REALLOCATIONS], l[LARGE_REALLOCATION_NANOS] / NANOS_PER_MICRO));
    }
}

/// Paints and presents, timing each half.
fn paint_and_present(view: &WebView, context: &SwglContext) -> (Duration, Duration) {
    let painted = Instant::now();
    view.paint();
    let presented = Instant::now();
    context.present();
    (presented - painted, presented.elapsed())
}

/// Frames painted while a page loads (the batch loop otherwise paints only
/// once it has loaded). Reports the first paint once, from the load request.
pub struct LoadPaints {
    requested: Instant,
    painted_frames: u32,
    first_reported: bool,
    samples: Vec<Sample>,
}

impl LoadPaints {
    pub fn new(requested: Instant) -> Self {
        Self { requested, painted_frames: 0, first_reported: false, samples: Vec::new() }
    }

    pub fn turn(&mut self, view: &WebView, context: &SwglContext, delegate: &Delegate) {
        let frames = delegate.frames.get();
        if frames == self.painted_frames {
            return;
        }
        self.painted_frames = frames;
        let (paint, present) = paint_and_present(view, context);
        self.samples.push(Sample { ready: Duration::ZERO, paint, present, large: [0; LARGE_COUNTERS] });
        if !self.first_reported {
            self.first_reported = true;
            say(&format!("PENNY-LOAD: stage=FirstPaint elapsed_ms={}",
                self.requested.elapsed().as_millis()));
        }
    }

    pub fn report(&self) {
        for (step, sample) in self.samples.iter().enumerate() {
            sample.report("load-paint", step);
        }
    }
}

/// Turns the event loop until the page title is the marker the wait-title
/// flag names (the JavaScript benchmark's completion), painting new frames.
pub fn wait_for_title(servo: &servo::Servo, view: &WebView, context: &SwglContext, delegate: &Delegate) {
    let Ok(marker) = std::fs::read_to_string(flag_path("bench-wait-title")) else { return; };
    let marker = marker.trim();
    let started = Instant::now();
    let mut painted = delegate.frames.get();
    while view.page_title().as_deref() != Some(marker) {
        if started.elapsed() > TITLE_DEADLINE {
            say(&format!("PENNY-BENCH: title {marker} timeout"));
            return;
        }
        spin_servo(servo);
        if delegate.frames.get() != painted {
            painted = delegate.frames.get();
            paint_and_present(view, context);
        }
        std::thread::park_timeout(PARK);
    }
    say(&format!("PENNY-BENCH: title {marker} after_ms={}", started.elapsed().as_millis()));
}

/// One input: send it, wait for Servo's next frame, paint and present it,
/// then absorb follow-up frames without measuring them.
fn step(servo: &servo::Servo, view: &WebView, context: &SwglContext, delegate: &Delegate,
        send: &dyn Fn()) -> Option<Sample> {
    let before = delegate.frames.get();
    let counted = large_counters();
    let sent = Instant::now();
    send();
    loop {
        spin_servo(servo);
        if delegate.frames.get() != before { break; }
        if sent.elapsed() > STEP_DEADLINE { return None; }
        std::thread::park_timeout(PARK);
    }
    let ready = sent.elapsed();
    let (paint, present) = paint_and_present(view, context);
    let mut seen = delegate.frames.get();
    let mut quiet = Instant::now();
    while quiet.elapsed() < STEP_QUIET {
        spin_servo(servo);
        if delegate.frames.get() != seen {
            seen = delegate.frames.get();
            paint_and_present(view, context);
            quiet = Instant::now();
        }
        std::thread::park_timeout(PARK);
    }
    let now = large_counters();
    let large = std::array::from_fn(|index| now[index].wrapping_sub(counted[index]));
    Some(Sample { ready, paint, present, large })
}

fn run(kind: &str, steps: usize, servo: &servo::Servo, view: &WebView, context: &SwglContext,
       delegate: &Delegate, send: &dyn Fn()) {
    let samples: Vec<Option<Sample>> =
        (0..steps).map(|_| step(servo, view, context, delegate, send)).collect();
    for (index, sample) in samples.iter().enumerate() {
        match sample {
            Some(sample) => sample.report(kind, index),
            None => say(&format!("PENNY-BENCH: kind={kind} step={index} timeout")),
        }
    }
}

/// Keys, then wheel scrolling, at the viewport's centre.
pub fn interact(servo: &servo::Servo, view: &WebView, context: &SwglContext, delegate: &Delegate) {
    let size = context.size();
    let scale = view.hidpi_scale_factor().0;
    say(&format!("PENNY-BENCH: viewport={}x{} scale={scale}", size.width, size.height));
    let centre = DevicePoint::new(size.width as f32 / 2.0, size.height as f32 / 2.0);
    view.focus();
    if crate::flag("alloc-timing") {
        // Safety: a flag store in the allocator wrapper.
        unsafe { servo_large_allocations_enable(true) };
    }
    save_frame(context, "top");
    run("key", KEY_STEPS, servo, view, context, delegate, &|| {
        for state in [KeyState::Down, KeyState::Up] {
            view.notify_input_event(InputEvent::Keyboard(KeyboardEvent::new_without_event(
                state, Key::Character(KEY.into()), Code::KeyA, Location::Standard,
                Modifiers::empty(), false, false)));
        }
    });
    save_frame(context, "keys");
    run("scroll", SCROLL_STEPS, servo, view, context, delegate, &|| {
        view.notify_input_event(InputEvent::Wheel(WheelEvent::new(
            WheelDelta { x: 0.0, y: WHEEL_NOTCH_DOWN_PIXELS, z: 0.0, mode: WheelMode::DeltaPixel },
            centre.into())));
    });
    save_frame(context, "scrolled");
}

/// Servo writes its time profile during shutdown; print it to the log.
/// With the bench-frames flag, the presented frame as a PPM beside the time
/// profile (rendering must stay identical across performance changes).
fn save_frame(context: &SwglContext, name: &str) {
    if !crate::flag("bench-frames") { return; }
    let size = context.size();
    let rect = servo::DeviceIntRect::from_origin_and_size(
        servo::DeviceIntPoint::zero(), servo::DeviceIntSize::new(size.width as i32, size.height as i32));
    let Some(image) = context.read_to_image(rect) else { return; };
    let path = crate::profile_path().with_file_name(format!("penny-{name}.ppm"));
    match crate::write_ppm(&path.to_string_lossy(), &image) {
        Ok(()) => say(&format!("PENNY-BENCH: frame {name} saved")),
        Err(error) => say(&format!("PENNY-BENCH: frame {name} not saved: {error}")),
    }
}

pub fn report_profile() {
    match std::fs::read_to_string(crate::profile_path()) {
        Ok(profile) => {
            for line in profile.lines() {
                say(&format!("PENNY-PROFILE: {line}"));
            }
        },
        Err(error) => say(&format!("PENNY-PROFILE: unavailable: {error}")),
    }
}
