//! Rate limiter for progress events so a chatty transfer does not flood the UI.

use std::time::{Duration, Instant};

pub(crate) struct Throttle {
    last: Option<Instant>,
    every: Duration,
}

impl Default for Throttle {
    fn default() -> Self {
        Self::new(Duration::from_millis(50))
    }
}

impl Throttle {
    pub(crate) fn new(every: Duration) -> Self {
        Self { last: None, every }
    }

    /// True if enough time has passed since the last accepted call.
    pub(crate) fn ready(&mut self) -> bool {
        match self.last {
            Some(t) if t.elapsed() < self.every => false,
            _ => {
                self.last = Some(Instant::now());
                true
            }
        }
    }
}
