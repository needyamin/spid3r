//! Execution budget: a shared step allowance that bounds how much work an
//! untrusted script may do before the engine stops it.
//!
//! Both script engines enforce it — the tree interpreter charges one step per
//! statement (and per loop iteration), the VM charges in batches at its
//! instruction poll — and the native AOT backend does not (a counter in
//! generated machine code is not free; embedders that run untrusted code use
//! the tree or VM engine). Exhaustion is a **fatal** runtime error: it
//! bypasses `try`/`catch` in both engines, so a script cannot swallow the
//! stop and keep running. See `docs/SECURITY.md`.
//!
//! The counter is shared by clone, so a script's spawned tasks draw from the
//! same allowance as the code that spawned them.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// A shared, saturating step counter. `None` on an engine means "no limit"
/// and costs one `Option` check per charge site.
#[derive(Clone, Debug)]
pub struct Budget {
    remaining: Arc<AtomicI64>,
}

impl Budget {
    /// A budget of `steps` units (statements in the tree engine; a VM poll
    /// batch is `2048` instructions).
    pub fn new(steps: u64) -> Self {
        let steps = steps.min(i64::MAX as u64) as i64;
        Budget {
            remaining: Arc::new(AtomicI64::new(steps)),
        }
    }

    /// Charge `steps` against the budget; `false` once it is exhausted. A
    /// charge that lands exactly on zero succeeds (the work was allowed); the
    /// *next* charge fails. The counter floors at zero, so repeated charges
    /// after exhaustion cannot wrap it back to a positive value.
    #[inline]
    pub fn charge(&self, steps: i64) -> bool {
        let steps = steps.max(0);
        let charged = self
            .remaining
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                if cur <= 0 {
                    None
                } else {
                    Some((cur - steps).max(0))
                }
            });
        charged.is_ok()
    }

    /// Units left (never below zero; a concurrent over-charge may read as a
    /// small negative number, which callers treat as exhausted).
    pub fn remaining(&self) -> i64 {
        self.remaining.load(Ordering::Relaxed).max(0)
    }

    /// `true` when no units remain.
    pub fn exhausted(&self) -> bool {
        self.remaining.load(Ordering::Relaxed) <= 0
    }
}

#[cfg(test)]
mod tests {
    use super::Budget;

    #[test]
    fn charges_down_and_floors_at_zero() {
        let b = Budget::new(10);
        assert!(b.charge(4));
        assert_eq!(b.remaining(), 6);
        assert!(b.charge(6));
        assert_eq!(b.remaining(), 0);
        assert!(!b.charge(1));
        assert!(b.exhausted());
        // Repeated charges cannot resurrect the budget.
        for _ in 0..100 {
            assert!(!b.charge(1));
        }
        assert_eq!(b.remaining(), 0);
    }

    #[test]
    fn clones_share_one_allowance() {
        let b = Budget::new(100);
        let c = b.clone();
        assert!(c.charge(60));
        assert_eq!(b.remaining(), 40);
        assert!(b.charge(40));
        assert!(!c.charge(1));
    }

    #[test]
    fn zero_budget_is_immediately_exhausted() {
        let b = Budget::new(0);
        assert!(b.exhausted());
        assert!(!b.charge(1));
    }
}
