/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Penny on CuBit: a bound on the media players alive at once.
//!
//! Every media element that loads a resource gets its own GStreamer player,
//! and each player brings threads: GstPlay's loop, the source's streaming
//! thread, the demuxer's queues, the decoders. CuBit gives a process a fixed
//! number of threads (the kernel's MAX_THREADS_PER_PROCESS, 128), and
//! slack.com's 28 autoplaying videos took Penny past it (WEB-001: 28 GstPlay,
//! 28 appsrc, 8 multiqueue and a dozen GLib pool threads at the refused
//! spawn), so the next thread Servo needed (a script thread) could not start
//! and the Constellation panicked.
//!
//! A media element that would exceed the bound waits instead, as a download
//! the user agent has suspended: networkState IDLE, a `suspend` event, and
//! the document's load event no longer delayed. Whenever a player is
//! released every waiting element is told; each tries again, and the ones
//! that find no room wait again. Hosted builds have no bound.

use std::collections::VecDeque;
use std::sync::Mutex;

/// Players alive at once in the whole process. Each costs about four
/// threads while it streams, so the bound keeps media within about 32 of
/// the kernel's 128 (Servo itself runs 45 to 60 at a site's start).
#[cfg(target_os = "cubit")]
pub(crate) const MAXIMUM_LIVE_PLAYERS: usize = 8;
#[cfg(not(target_os = "cubit"))]
pub(crate) const MAXIMUM_LIVE_PLAYERS: usize = usize::MAX;

type Wake = Box<dyn FnOnce() + Send>;

struct Budget {
    live: usize,
    waiting: VecDeque<Wake>,
}

static BUDGET: Mutex<Budget> = Mutex::new(Budget { live: 0, waiting: VecDeque::new() });

fn budget() -> std::sync::MutexGuard<'static, Budget> {
    BUDGET.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One live player's share of the bound; released on drop.
#[derive(Debug)]
pub(crate) struct PlayerSlot(());

impl Drop for PlayerSlot {
    fn drop(&mut self) {
        let waiting = {
            let mut budget = budget();
            budget.live -= 1;
            std::mem::take(&mut budget.waiting)
        };
        // Outside the lock: each queues a task to its own element.
        for wake in waiting {
            wake();
        }
    }
}

/// A slot now, or `None` after registering `wake`, which is called (on
/// whichever thread releases a player) when there may be room again.
pub(crate) fn admit(wake: impl FnOnce() + Send + 'static) -> Option<PlayerSlot> {
    let mut budget = budget();
    if budget.live < MAXIMUM_LIVE_PLAYERS {
        budget.live += 1;
        return Some(PlayerSlot(()));
    }
    budget.waiting.push_back(Box::new(wake));
    let (live, waiting) = (budget.live, budget.waiting.len());
    drop(budget);
    // Diagnostics (WEB-001): one line per waiting element.
    eprintln!("PENNY-MEDIA players={live} limit={MAXIMUM_LIVE_PLAYERS} waiting={waiting}");
    None
}
