// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jon Andrew
// Part of Penny, CuBit's web browser. Not derived from Servo's servoshell.

//! The WEB-001 compatibility survey (`compat` flag, tests/servo/compat/).
//! The same steps run here (hosted Linux and CuBit) and in the reference
//! browsers. The plan (`compat-plan`, one tab-separated step per line) loads
//! real sites, each in a fresh WebView with the survey probe
//! (`compat-probe`) installed before the page's scripts, then evaluates
//! scripts, scrolls, clicks and types as the plan says. Results go to the log
//! as `PENNY-COMPAT` records; screenshots are PPM files beside the profile.
//!
//! Steps:
//!   load <site> <deadline ms> <url>   navigate; wait for load or the deadline
//!   wait <ms>                         keep painting frames
//!   js <name> <script>                evaluate; record the result
//!   click <name> <script>             evaluate to "x,y" (CSS px); click there
//!   wheel <notches>                   scroll down at the viewport's centre
//!   keys <text>                       type the characters
//!   shot <name>                       save the presented frame

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use servo::{
    Code, DevicePoint, InputEvent, JSValue, JavaScriptEvaluationError, Key, KeyState,
    KeyboardEvent, Location, Modifiers, MouseButton, MouseButtonAction, MouseButtonEvent,
    MouseMoveEvent, RenderingContext, UserContentManager, UserScript, WebView, WebViewBuilder,
    WheelDelta, WheelEvent, WheelMode,
};
use url::Url;

use crate::{Delegate, SwglContext, flag_path, say, spin_servo};

/// A script that has not answered by then is recorded as a timeout.
const SCRIPT_DEADLINE: Duration = Duration::from_secs(10);
/// The longest the loop sleeps between event-loop turns (the waker unparks
/// it as soon as Servo has work).
const PARK: Duration = Duration::from_millis(1);
/// One wheel notch toward the page's end; Servo scrolls by the inverse of
/// the wheel delta, so negative is down (as in bench.rs).
const WHEEL_NOTCH_DOWN_PIXELS: f64 = -76.0;
/// Pause between wheel notches, as a hand on a wheel.
const WHEEL_NOTCH_GAP: Duration = Duration::from_millis(50);
/// Pause between typed characters.
const KEY_GAP: Duration = Duration::from_millis(30);
/// Log records are split into chunks no longer than this (the console write
/// takes bounded lines); the runner joins them.
const RECORD_CHUNK_BYTES: usize = 768;
/// Console errors kept per site, and their length.
pub const MAXIMUM_CONSOLE_LINES: usize = 20;
pub const MAXIMUM_CONSOLE_CHARACTERS: usize = 300;

enum Step {
    Load { site: String, deadline: Duration, url: String },
    Wait(Duration),
    Script { name: String, script: String },
    Click { name: String, script: String },
    Wheel(u32),
    Keys(String),
    Shot(String),
}

fn parse(plan: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (number, line) in plan.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let fields: Vec<&str> = line.splitn(4, '\t').collect();
        let bad = || format!("plan line {}: {line:.80}", number + 1);
        let millis = |text: &str| text.parse::<u64>().map(Duration::from_millis).map_err(|_| bad());
        let step = match fields.as_slice() {
            ["load", site, deadline, url] => Step::Load {
                site: site.to_string(), deadline: millis(deadline)?, url: url.to_string(),
            },
            ["wait", ms] => Step::Wait(millis(ms)?),
            ["js", name, script] => Step::Script { name: name.to_string(), script: script.to_string() },
            ["click", name, script] => Step::Click { name: name.to_string(), script: script.to_string() },
            ["wheel", notches] => Step::Wheel(notches.parse().map_err(|_| bad())?),
            ["keys", text] => Step::Keys(text.to_string()),
            ["shot", name] => Step::Shot(name.to_string()),
            _ => return Err(bad()),
        };
        steps.push(step);
    }
    Ok(steps)
}

/// One record: `PENNY-COMPAT <kind> <site> <name> <index>/<count> <chunk>`.
pub fn record(kind: &str, site: &str, name: &str, text: &str) {
    let clean: String = text.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
    let mut chunks = Vec::new();
    let mut rest = clean.as_str();
    while rest.len() > RECORD_CHUNK_BYTES {
        let mut cut = RECORD_CHUNK_BYTES;
        while !rest.is_char_boundary(cut) { cut -= 1; }
        chunks.push(&rest[..cut]);
        rest = &rest[cut..];
    }
    chunks.push(rest);
    let count = chunks.len();
    for (index, chunk) in chunks.iter().enumerate() {
        say(&format!("PENNY-COMPAT {kind} {site} {name} {}/{count} {chunk}", index + 1));
    }
}

struct Run<'a> {
    servo: &'a servo::Servo,
    context: &'a Rc<SwglContext>,
    delegate: &'a Rc<Delegate>,
    manager: Rc<UserContentManager>,
    view: Option<WebView>,
    site: String,
    painted: u32,
}

impl Run<'_> {
    /// One event-loop turn, painting and presenting a new frame.
    fn turn(&mut self) {
        spin_servo(self.servo);
        if let Some(view) = &self.view {
            if self.delegate.frames.get() != self.painted {
                self.painted = self.delegate.frames.get();
                view.paint();
                self.context.present();
            }
        }
        std::thread::park_timeout(PARK);
    }

    fn wait(&mut self, length: Duration) {
        let started = Instant::now();
        while started.elapsed() < length { self.turn(); }
    }

    fn load(&mut self, site: &str, deadline: Duration, url: &str) {
        self.finish_site();
        self.site = site.to_string();
        let Ok(url) = Url::parse(url) else {
            record("load", site, "-", "{\"loaded\":false,\"error\":\"not a URL\"}");
            return;
        };
        // A fresh WebView per site: no previous document's state or frames.
        self.view = None;
        #[cfg(target_os = "cubit")]
        crate::thread_census::report(site);
        self.delegate.begin_load();
        self.delegate.frames.set(0);
        self.painted = 0;
        self.delegate.console_errors.set(0);
        self.delegate.console_lines.borrow_mut().clear();
        *self.delegate.crashed.borrow_mut() = None;
        let requested = Instant::now();
        let view = WebViewBuilder::new(self.servo, self.context.clone())
            .delegate(self.delegate.clone())
            .user_content_manager(self.manager.clone())
            .url(url)
            .build();
        #[cfg(target_os = "cubit")]
        crate::synchronize_window(&view, self.context, self.delegate);
        self.view = Some(view);
        let mut first_frame = None;
        while !self.delegate.loaded.get() && requested.elapsed() < deadline
            && self.delegate.crashed.borrow().is_none() {
            let before = self.painted;
            self.turn();
            if self.painted != before && first_frame.is_none() {
                first_frame = Some(requested.elapsed());
            }
        }
        let millis = |d: Option<Duration>| d.map_or("null".to_string(), |d| d.as_millis().to_string());
        record("load", site, "-", &format!(
            "{{\"loaded\":{},\"load_ms\":{},\"first_frame_ms\":{},\"crashed\":{}}}",
            self.delegate.loaded.get(),
            millis(self.delegate.loaded.get().then(|| requested.elapsed())),
            millis(first_frame),
            self.delegate.crashed.borrow().is_some()));
    }

    /// Console errors and a crash report for the site just finished.
    fn finish_site(&mut self) {
        if self.site.is_empty() { return; }
        let lines = self.delegate.console_lines.borrow().clone();
        record("console", &self.site, "count", &self.delegate.console_errors.get().to_string());
        for (index, line) in lines.iter().enumerate() {
            record("console", &self.site, &index.to_string(), line);
        }
        if let Some(reason) = self.delegate.crashed.borrow().as_ref() {
            record("crash", &self.site, "-", reason);
        }
    }

    fn evaluate(&mut self, script: &str) -> Result<String, String> {
        let Some(view) = self.view.clone() else { return Err("no page".into()); };
        let answer: Rc<RefCell<Option<Result<JSValue, JavaScriptEvaluationError>>>> = Rc::default();
        let slot = answer.clone();
        view.evaluate_javascript(script, move |result| *slot.borrow_mut() = Some(result));
        let started = Instant::now();
        while answer.borrow().is_none() {
            if started.elapsed() > SCRIPT_DEADLINE { return Err("timeout".into()); }
            self.turn();
        }
        match answer.take().expect("answered") {
            Ok(JSValue::String(text)) => Ok(text),
            Ok(other) => Ok(format!("{other:?}")),
            Err(error) => Err(format!("{error:?}")),
        }
    }

    fn script(&mut self, name: &str, script: &str) {
        let (status, text) = match self.evaluate(script) {
            Ok(text) => ("ok", text),
            Err(error) => ("error", error),
        };
        record(&format!("js-{status}"), &self.site.clone(), name, &text);
    }

    fn click(&mut self, name: &str, script: &str) {
        let answer = self.evaluate(script);
        let site = self.site.clone();
        let point = answer.as_ref().ok().and_then(|text| {
            let (x, y) = text.split_once(',')?;
            Some((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?))
        });
        let (Some((x, y)), Some(view)) = (point, self.view.clone()) else {
            let text = answer.unwrap_or_else(|error| error);
            record("click-missed", &site, name, &text);
            return;
        };
        // Page coordinates are CSS pixels; input events take device pixels.
        let scale = view.hidpi_scale_factor().0;
        let point = DevicePoint::new(x * scale, y * scale);
        view.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point.into())));
        self.wait(WHEEL_NOTCH_GAP);
        for action in [MouseButtonAction::Down, MouseButtonAction::Up] {
            view.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                action, MouseButton::Primary, point.into())));
        }
        record("click", &site, name, &format!("{x},{y}"));
    }

    fn wheel(&mut self, notches: u32) {
        let Some(view) = self.view.clone() else { return; };
        let size = self.context.size();
        let centre = DevicePoint::new(size.width as f32 / 2.0, size.height as f32 / 2.0);
        for _ in 0..notches {
            view.notify_input_event(InputEvent::Wheel(WheelEvent::new(
                WheelDelta { x: 0.0, y: WHEEL_NOTCH_DOWN_PIXELS, z: 0.0, mode: WheelMode::DeltaPixel },
                centre.into())));
            self.wait(WHEEL_NOTCH_GAP);
        }
    }

    fn keys(&mut self, text: &str) {
        let Some(view) = self.view.clone() else { return; };
        for character in text.chars() {
            for state in [KeyState::Down, KeyState::Up] {
                view.notify_input_event(InputEvent::Keyboard(KeyboardEvent::new_without_event(
                    state, Key::Character(character.to_string()), Code::Unidentified,
                    Location::Standard, Modifiers::empty(), false, false)));
            }
            self.wait(KEY_GAP);
        }
    }

    fn shot(&mut self, name: &str) {
        let Some(view) = self.view.clone() else { return; };
        view.paint();
        self.context.present();
        let size = self.context.size();
        let rect = servo::DeviceIntRect::from_origin_and_size(
            servo::DeviceIntPoint::zero(),
            servo::DeviceIntSize::new(size.width as i32, size.height as i32));
        let site = self.site.clone();
        let Some(image) = self.context.read_to_image(rect) else {
            record("shot-failed", &site, name, "no frame");
            return;
        };
        let path = shot_path(&site, name);
        match crate::write_ppm(&path.to_string_lossy(), &image) {
            Ok(()) => record("shot", &site, name, &format!("{}x{}", image.width(), image.height())),
            Err(error) => record("shot-failed", &site, name, &error.to_string()),
        }
    }
}

/// Screenshots: beside the time profile (the writable scope on CuBit).
fn shot_path(site: &str, name: &str) -> std::path::PathBuf {
    crate::profile_path().with_file_name(format!("compat-{site}-{name}.ppm"))
}

/// Runs the plan, then returns (the caller shuts Servo down).
pub fn run(servo: &servo::Servo, context: &Rc<SwglContext>, delegate: &Rc<Delegate>) {
    let plan = std::fs::read_to_string(flag_path("compat-plan")).map_err(|e| e.to_string())
        .and_then(|text| parse(&text));
    let steps = match plan {
        Ok(steps) => steps,
        Err(error) => {
            say(&format!("PENNY-COMPAT plan-error - - 1/1 {error}"));
            return;
        },
    };
    let manager = Rc::new(UserContentManager::new(servo));
    match std::fs::read_to_string(flag_path("compat-probe")) {
        Ok(probe) => manager.add_script(Rc::new(UserScript::new(probe, None))),
        Err(error) => say(&format!("PENNY-COMPAT probe-missing - - 1/1 {error}")),
    }
    let size = context.size();
    say(&format!("PENNY-COMPAT viewport - - 1/1 {}x{}", size.width, size.height));
    let mut run = Run { servo, context, delegate, manager, view: None, site: String::new(), painted: 0 };
    for step in &steps {
        match step {
            Step::Load { site, deadline, url } => run.load(site, *deadline, url),
            Step::Wait(length) => run.wait(*length),
            Step::Script { name, script } => run.script(name, script),
            Step::Click { name, script } => run.click(name, script),
            Step::Wheel(notches) => run.wheel(*notches),
            Step::Keys(text) => run.keys(text),
            Step::Shot(name) => run.shot(name),
        }
    }
    run.finish_site();
    run.view = None;
    say("PENNY-COMPAT done - - 1/1 -");
}
