/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A new connection's real phases for Resource and Navigation Timing.
//!
//! Servo set its connection marks as placeholders around building the
//! request, before hyper connected (TODO(#21261)), and never set a
//! domainLookupEnd or a responseStart. Here each phase is marked as it
//! happens: the lookup (TimedResolver), the TCP connect (ServoHttpConnector),
//! the end of the TLS handshake (InstrumentedStream). The marks belong to
//! the connection (`TimedStream`, through hyper's `Connected` extras, which
//! hyper copies into every response on the connection); the first response
//! on the connection claims them for its fetch timing. Later requests on
//! the same connection keep the placeholder marks, which the specification
//! sets equal for a reused connection.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use hyper_util::client::legacy::connect::{Connected, Connection};
use parking_lot::Mutex;
use servo_base::cross_process_instant::CrossProcessInstant;
use tower::Service;

/// One connection's phases, as instants.
#[derive(Clone, Copy, Debug, Default)]
pub struct Phases {
    pub domain_lookup_start: Option<CrossProcessInstant>,
    pub domain_lookup_end: Option<CrossProcessInstant>,
    pub connect_start: Option<CrossProcessInstant>,
    pub secure_connection_start: Option<CrossProcessInstant>,
    pub connect_end: Option<CrossProcessInstant>,
}

/// A connection's phases, filled in while it connects; in every response's
/// extensions, where the first response on the connection claims them.
#[derive(Clone, Debug, Default)]
pub struct ConnectionTiming {
    phases: Arc<Mutex<Phases>>,
    claimed: Arc<AtomicBool>,
}

impl ConnectionTiming {
    /// The phases, for the first response on this connection only.
    pub fn claim(&self) -> Option<Phases> {
        (!self.claimed.swap(true, Ordering::AcqRel)).then(|| *self.phases.lock())
    }
}

tokio::task_local! {
    static CONNECTING: ConnectionTiming;
}

/// Mark a phase of the connection the running connect future makes.
/// Outside a recorded connect, nothing is marked.
pub fn mark(phase: impl FnOnce(&mut Phases, CrossProcessInstant)) {
    let now = CrossProcessInstant::now();
    let _ = CONNECTING.try_with(|timing| phase(&mut timing.phases.lock(), now));
}

/// Run a connect future with `timing` collecting its phases.
pub async fn record<F: Future>(timing: ConnectionTiming, future: F) -> F::Output {
    CONNECTING.scope(timing, future).await
}

/// The TLS handshake on the connection `connected` describes is done: its
/// connect ends now.
pub fn secure_connection_established(connected: &Connected) {
    let mut extensions = http::Extensions::new();
    connected.get_extras(&mut extensions);
    if let Some(timing) = extensions.get::<ConnectionTiming>() {
        timing.phases.lock().connect_end = Some(CrossProcessInstant::now());
    }
}

/// A transport stream with its connection's phases.
pub struct TimedStream<T> {
    inner: T,
    timing: ConnectionTiming,
}

impl<T> TimedStream<T> {
    pub fn new(inner: T, timing: ConnectionTiming) -> Self {
        Self { inner, timing }
    }
}

impl<T: Connection> Connection for TimedStream<T> {
    fn connected(&self) -> Connected {
        self.inner.connected().extra(self.timing.clone())
    }
}

impl<T: hyper::rt::Read + Unpin> hyper::rt::Read for TimedStream<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_read(context, buffer)
    }
}

impl<T: hyper::rt::Write + Unpin> hyper::rt::Write for TimedStream<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_write(context, buffer)
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(context)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[std::io::IoSlice<'_>],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(context, buffers)
    }
}

/// hyper's resolver with its lookup marked (domainLookupStart/End). The
/// TCP connect starts when the lookup ends.
#[derive(Clone)]
pub struct TimedResolver(hyper_util::client::legacy::connect::dns::GaiResolver);

impl TimedResolver {
    pub fn new() -> Self {
        Self(hyper_util::client::legacy::connect::dns::GaiResolver::new())
    }
}

type Name = hyper_util::client::legacy::connect::dns::Name;
type Addresses = hyper_util::client::legacy::connect::dns::GaiAddrs;

impl Service<Name> for TimedResolver {
    type Response = Addresses;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Addresses, std::io::Error>> + Send>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(context)
    }

    fn call(&mut self, name: Name) -> Self::Future {
        // The resolver is called from inside hyper's connect future, so the
        // task-local connection is in scope here and in the lookup's future.
        mark(|phases, now| phases.domain_lookup_start = Some(now));
        let lookup = self.0.call(name);
        Box::pin(async move {
            let addresses = lookup.await;
            mark(|phases, now| {
                phases.domain_lookup_end = Some(now);
                phases.connect_start = Some(now);
            });
            addresses
        })
    }
}
