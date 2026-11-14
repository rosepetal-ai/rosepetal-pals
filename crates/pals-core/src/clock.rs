//! The clock a node reads for its quotas, its records, its views and its
//! offers: the system's ([`SystemClock`]), or one the tests set and move
//! ([`ManualClock`]), so that nothing they check waits on elapsed time.

use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};

/// Where the time comes from. `Send + Sync`: share it behind an `Arc`.
pub trait Clock: Send + Sync {
    /// Unix seconds now.
    fn now_unix(&self) -> i64;
}

/// The system's clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }
}

/// A clock that stands still at the time it is given, until it is set or
/// moved.
pub struct ManualClock {
    now: AtomicI64,
}

impl ManualClock {
    /// A clock that reads `now_unix`.
    pub fn new(now_unix: i64) -> Self {
        Self {
            now: AtomicI64::new(now_unix),
        }
    }

    /// Reads `now_unix` from now on.
    pub fn set(&self, now_unix: i64) {
        self.now.store(now_unix, Ordering::SeqCst);
    }

    /// Moves the clock `seconds` forward (backward when negative).
    pub fn advance(&self, seconds: i64) {
        self.now.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_unix(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
}

impl fmt::Debug for ManualClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ManualClock")
            .field(&self.now_unix())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manual_clock_moves_only_when_told() {
        let clock = ManualClock::new(100);
        assert_eq!(clock.now_unix(), 100);
        clock.advance(60);
        assert_eq!(clock.now_unix(), 160);
        clock.set(5);
        assert_eq!(clock.now_unix(), 5);
        clock.advance(-5);
        assert_eq!(clock.now_unix(), 0);
        let system = SystemClock.now_unix();
        assert!((system - chrono::Utc::now().timestamp()).abs() <= 1);
    }
}
