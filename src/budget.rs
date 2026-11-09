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
//!
//! This module also holds the two `spid3r.ini` run-time limits that reuse
//! the same Arc-sharing pattern: a wall-clock deadline (`runtime.
//! wall_clock_limit_ms`, fatal on expiry) and a heap cap (`runtime.
//! memory_limit_mb`, catchable). Both are read fresh at every check, and
//! `config reload` pushes new values into the active handles
//! ([`refresh_runtime_limits`]), so a reload applies without a restart.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

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

// ------------------------------------------------------------- run-time limits

/// The deadline state shared by every engine of one run. The deadline is
/// measured against the process-wide monotonic clock (`sched::now_ms`), so a
/// check is one atomic load.
#[derive(Debug)]
struct WallClockInner {
    /// Monotonic ms deadline (`sched::now_ms()` units); `i64::MAX` = unlimited.
    deadline_ms: AtomicI64,
    /// The configured limit, kept so the fatal diagnostic can name it.
    limit_ms: AtomicI64,
}

/// A shared wall-clock deadline (`runtime.wall_clock_limit_ms`). Cloned into
/// spawned tasks and module interpreters, so the whole run draws one
/// deadline. Expiry is **fatal** in both engines: it bypasses `try`/`catch`,
/// like execution-budget exhaustion.
#[derive(Clone, Debug)]
pub struct WallClock {
    inner: Arc<WallClockInner>,
}

impl WallClock {
    /// A deadline `limit_ms` milliseconds from now. `0` means unlimited.
    pub fn new(limit_ms: u64) -> Self {
        let inner = Arc::new(WallClockInner {
            deadline_ms: AtomicI64::new(deadline_for(limit_ms)),
            limit_ms: AtomicI64::new(limit_ms.min(i64::MAX as u64) as i64),
        });
        *active_wall().lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&inner));
        WallClock { inner }
    }

    /// `true` once the monotonic clock has passed the deadline.
    #[inline]
    pub fn exceeded(&self) -> bool {
        crate::sched::now_ms() >= self.inner.deadline_ms.load(Ordering::Relaxed)
    }

    /// The configured limit in ms (for the fatal diagnostic).
    pub fn limit_ms(&self) -> i64 {
        self.inner.limit_ms.load(Ordering::Relaxed)
    }

    /// The fatal diagnostic for this limit: `wall-clock limit exceeded (N ms)`.
    pub fn message(&self) -> String {
        format!("wall-clock limit exceeded ({} ms)", self.limit_ms())
    }

    /// Swap the deadline in place (Live reload): every clone of this handle
    /// — tasks and module interpreters included — sees the new deadline.
    pub fn set_limit(&self, limit_ms: u64) {
        self.inner
            .limit_ms
            .store(limit_ms.min(i64::MAX as u64) as i64, Ordering::Relaxed);
        self.inner
            .deadline_ms
            .store(deadline_for(limit_ms), Ordering::Relaxed);
    }
}

fn deadline_for(limit_ms: u64) -> i64 {
    if limit_ms == 0 {
        i64::MAX
    } else {
        crate::sched::now_ms().saturating_add(limit_ms.min(i64::MAX as u64) as i64)
    }
}

/// A shared heap cap (`runtime.memory_limit_mb`), in MiB. Same Arc-sharing
/// pattern as [`WallClock`]; exceeding it is a **catchable** runtime error
/// (PHP-style): a program may catch the OOM and continue.
#[derive(Clone, Debug)]
pub struct MemLimit {
    cap_mb: Arc<AtomicI64>,
}

impl MemLimit {
    /// A cap of `cap_mb` MiB. `0` means unlimited.
    pub fn new(cap_mb: u64) -> Self {
        let cap = Arc::new(AtomicI64::new(cap_mb.min(i64::MAX as u64) as i64));
        *active_mem().lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&cap));
        MemLimit { cap_mb: cap }
    }

    /// The configured cap in MiB.
    pub fn cap_mb(&self) -> i64 {
        self.cap_mb.load(Ordering::Relaxed)
    }

    /// The catchable OOM diagnostic when the cap is exceeded, else `None`.
    /// `None` when unlimited — one comparison on the common path.
    pub fn check(&self) -> Option<String> {
        let cap = self.cap_mb();
        if cap <= 0 {
            return None;
        }
        crate::value::gc::check_memory_cap(cap)
    }

    /// Swap the cap in place (Live reload).
    pub fn set_cap(&self, cap_mb: u64) {
        self.cap_mb
            .store(cap_mb.min(i64::MAX as u64) as i64, Ordering::Relaxed);
    }
}

/// The handles of the currently-running program, registered by
/// [`WallClock::new`] / [`MemLimit::new`] and refreshed by
/// [`refresh_runtime_limits`] (from `config reload`). `None` when no run is
/// active — `config reload` then has nothing to update.
fn active_wall() -> &'static Mutex<Option<Arc<WallClockInner>>> {
    static ACTIVE: Mutex<Option<Arc<WallClockInner>>> = Mutex::new(None);
    &ACTIVE
}

fn active_mem() -> &'static Mutex<Option<Arc<AtomicI64>>> {
    static ACTIVE: Mutex<Option<Arc<AtomicI64>>> = Mutex::new(None);
    &ACTIVE
}

/// Read `runtime.wall_clock_limit_ms` / `runtime.memory_limit_mb` out of `cfg`.
fn limit_values(cfg: &crate::config::Config) -> (u64, u64) {
    let wall = cfg
        .get("runtime.wall_clock_limit_ms")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let mem = cfg
        .get("runtime.memory_limit_mb")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    (wall, mem)
}

/// Resolve the run-time limits from a config snapshot. Called once at run
/// start; the returned handles are installed into the engines and shared
/// with every task and module interpreter.
pub fn run_limits(cfg: &crate::config::Config) -> (Option<WallClock>, Option<MemLimit>) {
    let (wall_ms, mem_mb) = limit_values(cfg);
    (
        (wall_ms > 0).then(|| WallClock::new(wall_ms)),
        (mem_mb > 0).then(|| MemLimit::new(mem_mb)),
    )
}

/// Apply the configured wall-clock / memory limits to the active run's
/// handles (if any). Called by `config reload`; a run that started without a
/// limit cannot gain one this way, and a limit that was removed stays armed
/// until the process restarts — the handles, not the config snapshot, are
/// what the engines read.
pub fn refresh_runtime_limits(cfg: &crate::config::Config) {
    let (wall_ms, mem_mb) = limit_values(cfg);
    if let Some(inner) = active_wall()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        let limit = inner.limit_ms.load(Ordering::Relaxed);
        let new = if wall_ms > 0 {
            wall_ms.min(i64::MAX as u64) as i64
        } else {
            0
        };
        // Only rewrite when the value actually changed: `set_limit` moves the
        // deadline, and a reload must not silently extend an armed deadline.
        if limit != new {
            let wall = WallClock {
                inner: Arc::clone(inner),
            };
            wall.set_limit(new as u64);
        }
    }
    if let Some(cap) = active_mem()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        cap.store(mem_mb.min(i64::MAX as u64) as i64, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::{refresh_runtime_limits, run_limits, Budget, MemLimit, WallClock};
    use std::collections::BTreeMap;

    /// Build a config snapshot holding just the two runtime-limit keys.
    fn limits_cfg(wall_ms: &str, mem_mb: &str) -> crate::config::Config {
        let mut values = BTreeMap::new();
        values.insert(
            "runtime.wall_clock_limit_ms".to_string(),
            wall_ms.to_string(),
        );
        values.insert("runtime.memory_limit_mb".to_string(), mem_mb.to_string());
        crate::config::Config {
            values,
            path: None,
            notes: Vec::new(),
            mtime: None,
            pin_status: crate::config::PinStatus::Unpinned,
        }
    }

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

    /// `WallClock::new` registers the handle process-globally (for the reload
    /// path), so the cases below must not run concurrently with each other.
    static LIMIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn wall_clock_expires_and_is_swappable() {
        let _serial = LIMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let wc = WallClock::new(50);
        assert!(!wc.exceeded());
        assert_eq!(wc.limit_ms(), 50);
        assert_eq!(wc.message(), "wall-clock limit exceeded (50 ms)");
        // Swapping the deadline in place moves every clone.
        let clone = wc.clone();
        wc.set_limit(0);
        assert!(!clone.exceeded());
        assert_eq!(clone.limit_ms(), 0);
        // A fresh deadline in the past reports expiry.
        wc.set_limit(1);
        std::thread::sleep(std::time::Duration::from_millis(15));
        assert!(wc.exceeded());
    }

    #[test]
    fn mem_limit_checks_and_swaps() {
        let _serial = LIMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let m = MemLimit::new(0);
        assert_eq!(m.cap_mb(), 0);
        assert!(m.check().is_none(), "0 = unlimited");
        m.set_cap(64);
        assert_eq!(m.cap_mb(), 64);
        // A cap no heap can exceed stays silent.
        m.set_cap(i64::MAX as u64 / (1024 * 1024));
        assert!(m.check().is_none());
    }

    #[test]
    fn run_limits_resolves_from_config() {
        let _serial = LIMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Defaults: both unlimited.
        let (wall, mem) = run_limits(&limits_cfg("0", "0"));
        assert!(wall.is_none() && mem.is_none());
        let (wall, mem) = run_limits(&limits_cfg("500", "64"));
        assert_eq!(wall.as_ref().unwrap().limit_ms(), 500);
        assert_eq!(mem.as_ref().unwrap().cap_mb(), 64);
    }

    #[test]
    fn refresh_runtime_limits_updates_active_handles() {
        // `config reload` pushes new values into the Arc-shared handles the
        // engines hold: the Live classification of the two runtime keys.
        let _serial = LIMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let wc = WallClock::new(500);
        let mem = MemLimit::new(64);
        refresh_runtime_limits(&limits_cfg("750", "96"));
        assert_eq!(wc.limit_ms(), 750);
        assert_eq!(mem.cap_mb(), 96);
        // Removing a limit via reload disarms the deadline (0 = unlimited).
        refresh_runtime_limits(&limits_cfg("0", "0"));
        assert_eq!(wc.limit_ms(), 0);
        assert!(!wc.exceeded());
        assert_eq!(mem.cap_mb(), 0);
    }
}
