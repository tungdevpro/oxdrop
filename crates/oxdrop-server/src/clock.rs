//! Where the engine gets the time from, so tests can move it on by hand.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::watch;

/// The engine's only source of time: what time it is, and waiting for a later one.
pub trait Clock: Send + Sync + fmt::Debug + 'static {
    fn now(&self) -> Instant;

    /// Completes once [`now`](Self::now) has reached `deadline`.
    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// The real time.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep_until(deadline.into()))
    }
}

/// A clock that only moves when told to, for tests.
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: Arc<watch::Sender<Instant>>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            now: Arc::new(watch::Sender::new(Instant::now())),
        }
    }

    /// Moves the time on by `by`, waking anything waiting for it.
    pub fn advance(&self, by: Duration) {
        self.now.send_modify(|now| *now += by);
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        *self.now.borrow()
    }

    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let mut now = self.now.subscribe();
        Box::pin(async move {
            // The sender lives as long as the clock; if it's gone, nothing will move it.
            let _ = now.wait_for(|now| *now >= deadline).await;
        })
    }
}
