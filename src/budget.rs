//! Execution budget: a shared step allowance that bounds how much work an
//! untrusted script may do before the engine stops it.
//!
//! Both script engines enforce it — the tree interpreter charges one step per
//! statement (and per loop iteration), the VM charges in batches of `2048`
//! units per `2048`-dispatch poll (a fused superinstruction counts as one
//! dispatch even though it executes several original bytecode ops, preserving
//! a linear bound) — and the native AOT backend does not (a counter in
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

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

/// A shared, saturating step counter. `None` on an engine means "no limit"
/// and costs one `Option` check per charge site.
#[derive(Clone, Debug)]
pub struct Budget {
    remaining: Arc<AtomicI64>,
}

impl Budget {
    /// A budget of `steps` units (statements in the tree engine; in the VM,
    /// each poll batch charges `2048` units per `2048` opcode dispatches,
    /// where a fused superinstruction counts as one dispatch).
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
        {
            let mut slot = active_wall().lock().unwrap_or_else(|e| e.into_inner());
            if slot.1 == 0 {
                slot.0 = Some(Arc::clone(&inner));
                ACTIVE_WALL_ARMED.store(limit_ms > 0, Ordering::Release);
            }
        }
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
        let clamped = limit_ms.min(i64::MAX as u64) as i64;
        self.inner.limit_ms.store(clamped, Ordering::Relaxed);
        self.inner
            .deadline_ms
            .store(deadline_for(limit_ms), Ordering::Relaxed);
        let slot = active_wall().lock().unwrap_or_else(|e| e.into_inner());
        if slot
            .0
            .as_ref()
            .is_some_and(|cur| Arc::ptr_eq(cur, &self.inner))
        {
            ACTIVE_WALL_ARMED.store(clamped > 0, Ordering::Release);
        }
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
        let clamped = cap_mb.min(i64::MAX as u64) as i64;
        let cap = Arc::new(AtomicI64::new(clamped));
        {
            let mut slot = active_mem().lock().unwrap_or_else(|e| e.into_inner());
            if slot.1 == 0 {
                slot.0 = Some(Arc::clone(&cap));
                ACTIVE_MEM_CAP_MB.store(clamped, Ordering::Relaxed);
            }
        }
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
        let clamped = cap_mb.min(i64::MAX as u64) as i64;
        self.cap_mb.store(clamped, Ordering::Relaxed);
        let slot = active_mem().lock().unwrap_or_else(|e| e.into_inner());
        if slot
            .0
            .as_ref()
            .is_some_and(|cur| Arc::ptr_eq(cur, &self.cap_mb))
        {
            ACTIVE_MEM_CAP_MB.store(clamped, Ordering::Relaxed);
        }
    }
}

/// The handles of the currently-running program, registered by
/// [`WallClock::new`] / [`MemLimit::new`] and refreshed by
/// [`refresh_runtime_limits`] (from `config reload`). `None` when no run is
/// active — `config reload` then has nothing to update.
///
/// The second tuple element tracks the active `ActiveLimitGuard` nesting depth
/// so a guard entering with a handle pre-populated by `WallClock::new` /
/// `MemLimit::new` cleanly disarms the global slot on drop without clobbering
/// outer guards in nested runs (e.g. module imports).
fn active_wall() -> &'static Mutex<(Option<Arc<WallClockInner>>, usize)> {
    static ACTIVE: Mutex<(Option<Arc<WallClockInner>>, usize)> = Mutex::new((None, 0));
    &ACTIVE
}

fn active_mem() -> &'static Mutex<(Option<Arc<AtomicI64>>, usize)> {
    static ACTIVE: Mutex<(Option<Arc<AtomicI64>>, usize)> = Mutex::new((None, 0));
    &ACTIVE
}

fn active_budget_slot() -> &'static Mutex<Option<Budget>> {
    static ACTIVE: Mutex<Option<Budget>> = Mutex::new(None);
    &ACTIVE
}

static ACTIVE_MEM_CAP_MB: AtomicI64 = AtomicI64::new(0);
static ACTIVE_WALL_ARMED: AtomicBool = AtomicBool::new(false);

/// Default safety ceiling on a single string/bytes/container allocation (256 MiB)
/// when neither `memory.allocation_cap_mb` nor `runtime.memory_limit_mb` is set,
/// preventing Rust's global allocator from aborting the host process on runaway doubling.
pub const DEFAULT_SINGLE_ALLOC_CAP: usize = 256 * 1024 * 1024;

/// RAII guard that installs the active run's budget, wall-clock, and memory
/// limits for blocking natives / single-allocation checks, restoring the
/// previous state on exit so nested or sequential runs never leak or clobber
/// limits across runs.
///
/// Any `None` limit is left untouched rather than overwriting the global slot
/// with `None`, so an unlimited run (or concurrent test) never disarms an
/// already-active limit.
pub struct ActiveLimitGuard {
    prev_budget: Option<Option<Budget>>,
    /// `Some((prev_handle, restore_depth))`: `restore_depth` is `Some(d)` for
    /// `clear_for_test` (exact snapshot restore) or `None` for `enter`
    /// (decrement depth by 1).
    prev_wall: Option<(Option<Arc<WallClockInner>>, Option<usize>)>,
    prev_mem: Option<(Option<Arc<AtomicI64>>, i64, Option<usize>)>,
}

impl ActiveLimitGuard {
    pub fn enter(budget: Option<Budget>, wall: Option<&WallClock>, mem: Option<&MemLimit>) -> Self {
        let prev_budget = budget.map(|b| {
            active_budget_slot()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .replace(b)
        });
        let prev_wall = wall.map(|w| {
            let mut slot = active_wall().lock().unwrap_or_else(|e| e.into_inner());
            let prev = if slot.1 == 0 && slot.0.as_ref().is_some_and(|p| Arc::ptr_eq(p, &w.inner)) {
                // Pre-populated by `WallClock::new` outside any guard; disarm
                // back to `None` when this outermost guard exits.
                None
            } else {
                slot.0.take()
            };
            slot.0 = Some(Arc::clone(&w.inner));
            slot.1 += 1;
            ACTIVE_WALL_ARMED.store(w.limit_ms() > 0, Ordering::Release);
            (prev, None)
        });
        let prev_mem = mem.map(|m| {
            let mut slot = active_mem().lock().unwrap_or_else(|e| e.into_inner());
            let (prev, prev_mb) =
                if slot.1 == 0 && slot.0.as_ref().is_some_and(|p| Arc::ptr_eq(p, &m.cap_mb)) {
                    (None, 0)
                } else {
                    (slot.0.take(), ACTIVE_MEM_CAP_MB.load(Ordering::Relaxed))
                };
            slot.0 = Some(Arc::clone(&m.cap_mb));
            slot.1 += 1;
            ACTIVE_MEM_CAP_MB.store(m.cap_mb().max(0), Ordering::Relaxed);
            (prev, prev_mb, None)
        });
        ActiveLimitGuard {
            prev_budget,
            prev_wall,
            prev_mem,
        }
    }

    /// Save and explicitly clear all process-global limit slots for unit tests
    /// that exercise `WallClock::new` / `MemLimit::new` directly; restores the
    /// prior state on drop.
    #[cfg(test)]
    pub(crate) fn clear_for_test() -> Self {
        let prev_budget = active_budget_slot()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let prev_wall = {
            let mut slot = active_wall().lock().unwrap_or_else(|e| e.into_inner());
            ACTIVE_WALL_ARMED.store(false, Ordering::Release);
            let prev = slot.0.take();
            let depth = std::mem::replace(&mut slot.1, 0);
            (prev, Some(depth))
        };
        let prev_mem = {
            let mut slot = active_mem().lock().unwrap_or_else(|e| e.into_inner());
            let prev = slot.0.take();
            let depth = std::mem::replace(&mut slot.1, 0);
            let prev_mb = ACTIVE_MEM_CAP_MB.swap(0, Ordering::Relaxed);
            (prev, prev_mb, Some(depth))
        };
        ActiveLimitGuard {
            prev_budget: Some(prev_budget),
            prev_wall: Some(prev_wall),
            prev_mem: Some(prev_mem),
        }
    }
}

impl Drop for ActiveLimitGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev_budget.take() {
            *active_budget_slot()
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = prev;
        }
        if let Some((prev, restore_depth)) = self.prev_wall.take() {
            let mut slot = active_wall().lock().unwrap_or_else(|e| e.into_inner());
            slot.1 = match restore_depth {
                Some(d) => d,
                None => slot.1.saturating_sub(1),
            };
            let armed = prev
                .as_ref()
                .is_some_and(|w| w.limit_ms.load(Ordering::Relaxed) > 0);
            ACTIVE_WALL_ARMED.store(armed, Ordering::Release);
            slot.0 = prev;
        }
        if let Some((prev, prev_mb, restore_depth)) = self.prev_mem.take() {
            let mut slot = active_mem().lock().unwrap_or_else(|e| e.into_inner());
            slot.1 = match restore_depth {
                Some(d) => d,
                None => slot.1.saturating_sub(1),
            };
            slot.0 = prev;
            ACTIVE_MEM_CAP_MB.store(prev_mb, Ordering::Relaxed);
        }
    }
}

/// Register the currently executing engine's step budget (for blocking natives).
pub fn set_active_budget(budget: Option<Budget>) {
    *active_budget_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = budget;
}

/// Register the currently executing engine's heap limit handle.
pub fn set_active_mem_limit(limit: Option<&MemLimit>) {
    let mb = limit.map(|l| l.cap_mb().max(0)).unwrap_or(0);
    ACTIVE_MEM_CAP_MB.store(mb, Ordering::Relaxed);
}

/// Returns `false` if an active step budget exists and is exhausted.
pub fn charge_active_budget(steps: i64) -> bool {
    let guard = active_budget_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(b) => b.charge(steps),
        None => true,
    }
}

/// Returns `true` if an active wall-clock deadline has expired.
pub fn active_wall_exceeded() -> bool {
    active_wall_message().is_some()
}

/// Returns `true` if a wall-clock deadline is ARMED for this run (installed
/// by `ActiveLimitGuard`), whether or not it has expired yet. Blocking
/// primitives (`sleep_ms`, `mutex.lock`, `join`, `enqueue`) use this to decide
/// whether their wait must be time-sliced.
pub fn active_wall_armed() -> bool {
    ACTIVE_WALL_ARMED.load(Ordering::Acquire)
}

/// Returns the formatted wall-clock timeout message if the active deadline has expired.
pub fn active_wall_message() -> Option<String> {
    if !ACTIVE_WALL_ARMED.load(Ordering::Acquire) {
        return None;
    }
    let guard = active_wall().lock().unwrap_or_else(|e| e.into_inner());
    guard.0.as_ref().and_then(|inner| {
        if crate::sched::now_ms() >= inner.deadline_ms.load(Ordering::Relaxed) {
            Some(format!(
                "wall-clock limit exceeded ({} ms)",
                inner.limit_ms.load(Ordering::Relaxed)
            ))
        } else {
            None
        }
    })
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

/// Configured step budget from `memory.max_steps` (`0` = unlimited).
pub fn configured_max_steps(cfg: &crate::config::Config) -> Option<u64> {
    cfg.get("memory.max_steps")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
}

/// Configured call-depth limit from `memory.max_call_depth` (clamped `>= 16`).
pub fn configured_max_call_depth(cfg: &crate::config::Config) -> Option<usize> {
    cfg.get("memory.max_call_depth")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n >= 16)
}

static BAKED_ALLOC_CAP_MB: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// Soft cap on a single allocation in bytes from `memory.allocation_cap_mb`
/// (`0` = unlimited). Baked once per process so `config_set` cannot loosen it
/// mid-run.
pub fn allocation_cap_bytes() -> Option<usize> {
    let mb = *BAKED_ALLOC_CAP_MB.get_or_init(|| {
        crate::config::get("memory.allocation_cap_mb")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(0)
    });
    if mb == 0 {
        None
    } else {
        Some((mb as usize).saturating_mul(1024 * 1024))
    }
}

/// Effective byte ceiling for a single allocation under the current
/// `memory.allocation_cap_mb`, `runtime.memory_limit_mb`, and host-abort guard.
pub fn effective_single_allocation_cap() -> usize {
    let mut cap = allocation_cap_bytes().unwrap_or(DEFAULT_SINGLE_ALLOC_CAP);
    let mut mem_mb = ACTIVE_MEM_CAP_MB.load(Ordering::Relaxed);
    if mem_mb <= 0 {
        mem_mb = crate::config::get("runtime.memory_limit_mb")
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(0);
    }
    if mem_mb > 0 {
        let mem_cap = (mem_mb as usize).saturating_mul(1024 * 1024);
        cap = cap.min(mem_cap);
    }
    cap
}

/// Enforce `memory.allocation_cap_mb`, `runtime.memory_limit_mb`, and the
/// single-allocation host-abort safety cap before a large buffer/string/list
/// allocation. Returns a catchable runtime error instead of aborting the host.
pub fn check_single_allocation(bytes: usize) -> Result<(), String> {
    if let Some(cap) = allocation_cap_bytes() {
        if bytes > cap {
            return Err(format!(
                "allocation limit exceeded: requested {} bytes, memory.allocation_cap_mb allows at most {} bytes",
                bytes, cap
            ));
        }
    }
    let mut mem_mb = ACTIVE_MEM_CAP_MB.load(Ordering::Relaxed);
    if mem_mb <= 0 {
        mem_mb = crate::config::get("runtime.memory_limit_mb")
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(0);
    }
    if mem_mb > 0 {
        let cap_bytes = (mem_mb as usize).saturating_mul(1024 * 1024);
        if bytes > cap_bytes
            || crate::value::gc::cached_memory_usage_bytes().saturating_add(bytes) > cap_bytes
        {
            return Err(format!(
                "out of memory: script exceeded memory_limit_mb ({})",
                mem_mb
            ));
        }
    }
    if allocation_cap_bytes().is_none() && bytes > DEFAULT_SINGLE_ALLOC_CAP {
        return Err(format!(
            "out of memory: single allocation of {} bytes exceeds limit ({} bytes)",
            bytes, DEFAULT_SINGLE_ALLOC_CAP
        ));
    }
    Ok(())
}

/// Fast-path allocation check: skips global limit checks for tiny allocations
/// below 16 KiB so hot inner loops pay one branch instruction.
#[inline(always)]
pub fn check_alloc_fast(bytes: usize) -> Result<(), String> {
    if bytes < 16 * 1024 {
        return Ok(());
    }
    check_single_allocation(bytes)
}

/// Concatenate two strings with memory/allocation cap checks and `try_reserve_exact`.
#[inline]
pub fn concat_strings_checked(a: &str, b: &str) -> Result<String, String> {
    let total = a
        .len()
        .checked_add(b.len())
        .ok_or_else(|| "out of memory: string size overflow".to_string())?;
    check_alloc_fast(total)?;
    let mut s = String::new();
    s.try_reserve_exact(total)
        .map_err(|_| format!("out of memory: cannot allocate string of {} bytes", total))?;
    s.push_str(a);
    s.push_str(b);
    Ok(s)
}

/// Append `rhs` onto `buf` with memory/allocation cap checks and `try_reserve`.
#[inline]
pub fn append_string_checked(buf: &mut String, rhs: &str) -> Result<(), String> {
    let total = buf
        .len()
        .checked_add(rhs.len())
        .ok_or_else(|| "out of memory: string size overflow".to_string())?;
    check_alloc_fast(total)?;
    buf.try_reserve(rhs.len())
        .map_err(|_| format!("out of memory: cannot grow string to {} bytes", total))?;
    buf.push_str(rhs);
    Ok(())
}

/// Validate that growing a container of `T` by `extra` elements fits within
/// configured memory and allocation caps without overflow or allocator abort.
#[inline]
pub fn check_vec_grow<T>(cur_len: usize, extra: usize) -> Result<(), String> {
    let total_len = cur_len
        .checked_add(extra)
        .ok_or_else(|| "out of memory: container size overflow".to_string())?;
    let bytes = total_len
        .checked_mul(std::mem::size_of::<T>())
        .ok_or_else(|| "out of memory: container allocation overflow".to_string())?;
    check_alloc_fast(bytes)
}

/// Apply the configured wall-clock / memory limits to the active run's
/// handles (if any). Called by `config reload`; a run that started without a
/// limit cannot gain one this way, and a limit that was removed stays armed
/// until the process restarts — the handles, not the config snapshot, are
/// what the engines read.
pub fn refresh_runtime_limits(cfg: &crate::config::Config) {
    let (wall_ms, mem_mb) = limit_values(cfg);
    let active_w = active_wall()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .0
        .as_ref()
        .map(Arc::clone);
    if let Some(inner) = active_w {
        let limit = inner.limit_ms.load(Ordering::Relaxed);
        let new = if wall_ms > 0 {
            wall_ms.min(i64::MAX as u64) as i64
        } else {
            0
        };
        // Only rewrite when the value actually changed: `set_limit` moves the
        // deadline, and a reload must not silently extend an armed deadline.
        if limit != new {
            let wall = WallClock { inner };
            wall.set_limit(new as u64);
        }
    }
    if let Some(cap) = active_mem()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .0
        .as_ref()
    {
        let clamped = mem_mb.min(i64::MAX as u64) as i64;
        cap.store(clamped, Ordering::Relaxed);
        ACTIVE_MEM_CAP_MB.store(clamped, Ordering::Relaxed);
    }
}

#[cfg(test)]
pub(crate) mod tests {
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
    /// path), so tests that arm the global wall-clock slot must hold this lock.
    pub(crate) static LIMIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn wall_clock_expires_and_is_swappable() {
        let _serial = LIMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = super::ActiveLimitGuard::clear_for_test();
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
        let _restore = super::ActiveLimitGuard::clear_for_test();
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
        let _restore = super::ActiveLimitGuard::clear_for_test();
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
        let _restore = super::ActiveLimitGuard::clear_for_test();
        let wc = WallClock::new(500);
        let mem = MemLimit::new(64);
        refresh_runtime_limits(&limits_cfg("750", "96"));
        assert_eq!(wc.limit_ms(), 750);
        assert_eq!(mem.cap_mb(), 96);
        // Removing a limit via reload disarms the deadline (0 = unlimited).
        refresh_runtime_limits(&limits_cfg("0", "0"));
        assert_eq!(wc.limit_ms(), 0);
        assert!(!wc.exceeded());
        assert!(!super::active_wall_armed());
        assert_eq!(mem.cap_mb(), 0);
    }

    #[test]
    fn active_limit_guard_disarms_wall_and_mem_without_breaking_nesting() {
        let _serial = LIMIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = super::ActiveLimitGuard::clear_for_test();

        // WallClock::new(0) is unlimited and must not arm time-slicing.
        let unlimited = WallClock::new(0);
        assert!(!super::active_wall_armed());
        drop(unlimited);

        // Sequential run: WallClock::new + MemLimit::new followed by
        // ActiveLimitGuard::enter must disarm both slots on drop so a
        // subsequent unlimited run never inherits an expired deadline or stale
        // memory cap.
        let wc = WallClock::new(1);
        let mem = MemLimit::new(32);
        assert!(super::active_wall_armed());
        {
            let _outer = super::ActiveLimitGuard::enter(None, Some(&wc), Some(&mem));
            assert!(super::active_wall_armed());
            // Nested guard with cloned handles (e.g., module import) must keep
            // the outer run's limits armed after the nested guard drops.
            {
                let _inner = super::ActiveLimitGuard::enter(None, Some(&wc), Some(&mem));
                assert!(super::active_wall_armed());
            }
            assert!(
                super::active_wall_armed(),
                "inner guard drop must not disarm outer guard"
            );
            std::thread::sleep(std::time::Duration::from_millis(15));
            assert!(super::active_wall_message().is_some());
        }
        // Outermost guard exited: both wall clock and mem cap must be disarmed.
        assert!(
            !super::active_wall_armed(),
            "expired WallClock leaked past ActiveLimitGuard drop"
        );
        assert!(super::active_wall_message().is_none());
        assert_eq!(
            super::ACTIVE_MEM_CAP_MB.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "MemLimit leaked past ActiveLimitGuard drop"
        );
        // Subsequent unlimited run must succeed cleanly.
        let _unlimited_run = super::ActiveLimitGuard::enter(None, None, None);
        assert!(crate::sched::sleep_ms(1).is_ok());
    }
}
