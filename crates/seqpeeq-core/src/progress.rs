//! How far a slow piece of work has got, and a way to ask it to stop.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Shared between the thread doing the work and the one showing it. The
/// units are whatever the work counts in (bytes, rows); only the fraction
/// is meant to be shown.
#[derive(Debug, Default)]
pub struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    cancelled: AtomicBool,
}

impl Progress {
    pub fn new() -> Progress {
        Progress::default()
    }

    pub fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
    }

    pub fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    /// Done over total, at most 1.0; `None` until a total is known.
    pub fn fraction(&self) -> Option<f64> {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return None;
        }
        let done = self.done.load(Ordering::Relaxed).min(total);
        Some(done as f64 / total as f64)
    }

    /// Ask the work to stop. It notices at its next check.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_is_none_without_a_total_and_never_above_one() {
        let progress = Progress::new();
        assert_eq!(progress.fraction(), None);
        progress.set_total(4);
        progress.add(1);
        assert_eq!(progress.fraction(), Some(0.25));
        progress.add(5);
        assert_eq!(progress.fraction(), Some(1.0));
        assert!(!progress.is_cancelled());
        progress.cancel();
        assert!(progress.is_cancelled());
    }
}
