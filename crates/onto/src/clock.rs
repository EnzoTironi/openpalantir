//! Production wall clock vs frozen test clock.
//!
//! [`Clock::live`] reads `UNIX` seconds. [`Clock::frozen`] is the test
//! harness. [`Clock::set`] freezes a live clock (tests that opened a file).

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Process clock. Live or frozen; never both at once.
pub struct Clock {
    frozen: AtomicBool,
    value: AtomicI64,
}

impl Clock {
    /// Wall clock. Used by [`crate::Engine::from_store`] / [`crate::Engine::open`].
    #[must_use]
    pub fn live() -> Self {
        Self {
            frozen: AtomicBool::new(false),
            value: AtomicI64::new(0),
        }
    }

    /// Fixed instant. Used by [`crate::Engine::memory`].
    #[must_use]
    pub fn frozen(at: i64) -> Self {
        Self {
            frozen: AtomicBool::new(true),
            value: AtomicI64::new(at),
        }
    }

    #[must_use]
    pub fn now(&self) -> i64 {
        if self.frozen.load(Ordering::SeqCst) {
            self.value.load(Ordering::SeqCst)
        } else {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        }
    }

    pub fn set(&self, secs: i64) {
        self.value.store(secs, Ordering::SeqCst);
        self.frozen.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_frozen(&self) -> bool {
        self.frozen.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_keep_frozen_value_if_set() {
        let clock = Clock::frozen(1_700_000_000);
        assert!(clock.is_frozen());
        assert_eq!(clock.now(), 1_700_000_000);
        clock.set(42);
        assert_eq!(clock.now(), 42);
    }

    #[test]
    fn does_read_wall_clock_if_live() {
        let clock = Clock::live();
        assert!(!clock.is_frozen());
        let n = clock.now();
        assert!(
            n > 1_700_000_000,
            "live clock must not be the frozen teaching instant"
        );
    }
}
