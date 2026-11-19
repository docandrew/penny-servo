/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Penny on CuBit: a bound on the HTTP client's live TCP connections.
//!
//! netstack refuses a connection beyond the count Penny's manifest declares
//! (userspace/servo/manifest.ccl, `connections`). Hyper's client opens a new
//! HTTP/1.1 connection for every request that finds no idle one, with no
//! limit, so a page fetching a hundred resources at once used to take every
//! declared connection and have the rest refused. Here a connection is
//! opened only with a permit, at most `CONNECTIONS_PER_HOST` per host and
//! port (other browsers' limit) and `HTTP_CLIENT_CONNECTIONS` in all; a
//! request without one waits for a connection to close. The permit lives in
//! the connection's stream and is returned when hyper drops it.
//!
//! Connections hyper keeps idle in its pool hold their permits too (up to
//! its idle timeout), so a page reaching many hosts used to wait seconds
//! for other hosts' idle connections to time out (WEB-001: up to 7.6 s on
//! nasa.gov). When the whole share is in use, a waiting connect now evicts
//! one connection of a host with no request in flight (`InFlight`, counted
//! by http_loader from before the request until its body ends): the
//! connection reads end-of-file and refuses writes, hyper drops it, and its
//! permit goes to the waiter. Chromium's socket pools close idle sockets of
//! other groups the same way when they reach their limit.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::task::{Context, Waker};

use http::uri::Uri;
use tokio::sync::Notify;

/// The TCP connections Penny's manifest declares (`connections` in
/// userspace/servo/manifest.ccl); the two must change together.
pub const DECLARED_CONNECTIONS: usize = 32;
/// Kept back from the HTTP client: WebSockets, and connections hyper has
/// dropped whose close netstack has not yet finished.
pub const RESERVED_CONNECTIONS: usize = 8;
/// The HTTP client's whole share of the declaration.
pub const HTTP_CLIENT_CONNECTIONS: usize = DECLARED_CONNECTIONS - RESERVED_CONNECTIONS;
/// Per host and port, as Chromium and Firefox limit HTTP/1.1 (an HTTP/2
/// connection carries all of a host's requests on one).
pub const CONNECTIONS_PER_HOST: usize = 6;

const HTTP_PORT: u16 = 80;
const HTTPS_PORT: u16 = 443;

#[derive(Default)]
struct Counts {
    total: usize,
    hosts: HashMap<String, usize>,
    /// Requests in flight per host and port (`InFlight`).
    in_flight: HashMap<String, usize>,
    /// Every live connection, for eviction.
    connections: Vec<Weak<Connection>>,
}

/// A live connection's eviction state.
struct Connection {
    key: String,
    evicted: AtomicBool,
    /// The connection's reader, woken to see the eviction.
    reader: Mutex<Option<Waker>>,
}

static COUNTS: LazyLock<Mutex<Counts>> = LazyLock::new(Default::default);
static RELEASED: Notify = Notify::const_new();
static PEAK: AtomicUsize = AtomicUsize::new(0);
static WAITS: AtomicUsize = AtomicUsize::new(0);
static EVICTIONS: AtomicUsize = AtomicUsize::new(0);

/// One live connection's share; returned on drop.
pub struct Permit {
    key: String,
    connection: Arc<Connection>,
}

impl Permit {
    /// Whether this connection was evicted; if not, the reader in `context`
    /// is woken when it is.
    pub fn poll_evicted(&self, context: &mut Context<'_>) -> bool {
        if self.connection.evicted.load(Ordering::Acquire) {
            return true;
        }
        {
            let mut reader = self.connection.reader.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if !reader.as_ref().is_some_and(|waker| waker.will_wake(context.waker())) {
                *reader = Some(context.waker().clone());
            }
        }
        // An eviction between the first check and the registration.
        self.connection.evicted.load(Ordering::Acquire)
    }

    /// Whether this connection was evicted.
    pub fn evicted(&self) -> bool {
        self.connection.evicted.load(Ordering::Acquire)
    }
}

/// A request in flight to a host: its connections are not evicted.
pub struct InFlight {
    key: String,
}

impl InFlight {
    /// `host` and `port` as in the request's URL (the port its scheme's
    /// default when absent).
    pub fn new(host: &str, port: u16) -> std::sync::Arc<Self> {
        let key = format!("{host}:{port}");
        let mut counts = COUNTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *counts.in_flight.entry(key.clone()).or_default() += 1;
        std::sync::Arc::new(InFlight { key })
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        {
            let mut counts = COUNTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(count) = counts.in_flight.get_mut(&self.key) {
                *count -= 1;
                if *count == 0 {
                    counts.in_flight.remove(&self.key);
                }
            }
        }
        // The host's connections may now be evicted for a waiter.
        RELEASED.notify_waiters();
    }
}

/// Evict one connection of a host other than `key` with no request in
/// flight, unless an eviction is already under way. Called with the counts
/// locked, when the whole share is in use.
fn evict_one(counts: &mut Counts, key: &str) {
    counts.connections.retain(|connection| connection.strong_count() > 0);
    let live: Vec<Arc<Connection>> = counts.connections.iter().filter_map(Weak::upgrade).collect();
    if live.iter().any(|connection| connection.evicted.load(Ordering::Acquire)) {
        return;
    }
    let Some(victim) = live.iter().find(|connection| {
        connection.key != key && !counts.in_flight.contains_key(&connection.key)
    }) else {
        return;
    };
    victim.evicted.store(true, Ordering::Release);
    EVICTIONS.fetch_add(1, Ordering::Relaxed);
    if let Some(reader) = victim.reader.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
        reader.wake();
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        {
            let mut counts = COUNTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            counts.total -= 1;
            if let Some(count) = counts.hosts.get_mut(&self.key) {
                *count -= 1;
                if *count == 0 {
                    counts.hosts.remove(&self.key);
                }
            }
        }
        RELEASED.notify_waiters();
    }
}

/// The limit's key for a destination: host and port.
pub fn key_of(destination: &Uri) -> String {
    let default_port = if destination.scheme_str() == Some("http") { HTTP_PORT } else { HTTPS_PORT };
    format!(
        "{}:{}",
        destination.host().unwrap_or_default(),
        destination.port_u16().unwrap_or(default_port)
    )
}

/// A wait for a permit this long or longer is logged (diagnostics, WEB-001).
const SLOW_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

/// Wait for room for one more connection to `key`'s host.
pub async fn acquire(key: String) -> Permit {
    let mut waited = false;
    let started = std::time::Instant::now();
    loop {
        let released = RELEASED.notified();
        tokio::pin!(released);
        // Registered before the check: a release between the check and the
        // wait still wakes this one.
        released.as_mut().enable();
        {
            let mut counts = COUNTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let host = counts.hosts.get(&key).copied().unwrap_or(0);
            if counts.total < HTTP_CLIENT_CONNECTIONS && host < CONNECTIONS_PER_HOST {
                counts.total += 1;
                *counts.hosts.entry(key.clone()).or_default() += 1;
                let live = counts.total;
                let connection = Arc::new(Connection {
                    key: key.clone(),
                    evicted: AtomicBool::new(false),
                    reader: Mutex::new(None),
                });
                counts.connections.push(Arc::downgrade(&connection));
                drop(counts);
                let waited_for = started.elapsed();
                if waited_for >= SLOW_WAIT {
                    eprintln!(
                        "PENNY-NET waited {} ms for {key}: total={live} host={} evictions={}",
                        waited_for.as_millis(),
                        host + 1,
                        EVICTIONS.load(Ordering::Relaxed)
                    );
                }
                if live > PEAK.fetch_max(live, Ordering::Relaxed) {
                    // Diagnostics (WEB-001): at most HTTP_CLIENT_CONNECTIONS lines.
                    eprintln!(
                        "PENNY-NET connections peak={live} limit={HTTP_CLIENT_CONNECTIONS} waits={}",
                        WAITS.load(Ordering::Relaxed)
                    );
                }
                return Permit { key, connection };
            }
            if counts.total >= HTTP_CLIENT_CONNECTIONS {
                evict_one(&mut counts, &key);
            }
        }
        if !waited {
            waited = true;
            WAITS.fetch_add(1, Ordering::Relaxed);
        }
        released.await;
    }
}
