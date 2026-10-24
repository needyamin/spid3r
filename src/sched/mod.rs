//! The task scheduler. Tasks run on a pool of OS threads (default size:
//! available parallelism) over one shared ready queue. There is no global
//! interpreter lock: two CPU-bound tasks run on two OS threads at once.
//!
//! Concurrency contract:
//! - `recv` on a channel with no future sender blocks the calling task
//!   forever (it parks on a condvar, so it does not spin). While tasks are
//!   parked the pool grows on demand, so blocked tasks never starve queued
//!   work. The pool is capped by `SPID3R_MAX_WORKERS` (default 256); a stall
//!   at that ceiling is reported on stderr by a watchdog instead of hanging
//!   silently (see `watchdog_loop`).
//! - **deadlock detection**: a wait that only another task can end
//!   (`recv`/`mutex.lock`/`join`) registers as an *untimed park*. When no task
//!   is running, queued, or able to run and a thread is parked that way, the
//!   watchdog aborts with a diagnostic instead of hanging forever
//!   (`SPID3R_DEADLOCK_ABORT=0` downgrades to a warning). Socket waits and
//!   `sleep_ms` are timed parks and never trigger it.
//! - a `mutex` is **not reentrant**: a task locking one it already holds gets
//!   a catchable `mutex: deadlock` error rather than parking forever.
//! - the ready queue is **bounded** (`SPID3R_QUEUE_CAP`, default 65536 jobs).
//!   `spawn` applies backpressure: it grows the pool first and then parks the
//!   producer until a worker takes a job, so a spawn storm cannot exhaust
//!   memory.
//! - `await` on a task joins it. Idle workers above the base pool retire
//!   after `RETIRE_AFTER`.
//! - program exit waits up to `DEFAULT_DRAIN_MS` (`SPID3R_TASK_DRAIN_MS`) for
//!   tasks that `main` left running, so their output is not lost to process
//!   teardown; tasks still running after the deadline are reported and the
//!   process exits anyway.
//! - [`Scheduler::shutdown`] stops and joins the worker threads. The CLI calls
//!   it once, after the last program, so process teardown is deterministic;
//!   the singleton restarts if something spawns afterwards (REPL/embed).
//! - a task that fails while holding a mutex releases it when its scope ends
//!   (locks are scope-based, like `finally`).
//!
//! GC safety: [`active_threads`] counts threads that are *executing* Spid3r
//! code, excluding those parked in a known blocking wait (`recv`, `join`,
//! `mutex.lock`, `sleep_ms`). The collector runs only when the caller is the
//! only active thread, which is the real precondition for a race-free
//! mark-sweep (see `gc::maybe_collect`).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
#[cfg(not(target_arch = "wasm32"))]
use std::thread;
#[cfg(not(target_arch = "wasm32"))]
use std::thread::JoinHandle;

use crate::value::Value;

/// What a spawned job produces: the task's result plus, when the task failed
/// by throwing a user value, the original thrown value — the joining engine
/// rethrows it structurally so `catch` rebinds the value itself, not its
/// display string (audit A-9).
pub struct TaskOutcome {
    pub result: Result<Value, String>,
    pub thrown: Option<Value>,
}

// ------------------------------------------------------- active-thread accounting

/// Threads currently executing Spid3r code, i.e. not parked in a blocking wait
/// this module knows about. Used by the GC gate: a mark-sweep is race-free
/// exactly when no other thread can be mutating values, which is the case when
/// the caller is the only *active* thread — parked threads hold their values on
/// their own stacks (external references), so those values stay roots.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Marks the current thread as executing Spid3r code until dropped.
pub struct ActiveGuard(());

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Declare that the current thread starts executing Spid3r code.
pub fn enter_active() -> ActiveGuard {
    ACTIVE.fetch_add(1, Ordering::SeqCst);
    ActiveGuard(())
}

/// Marks the current thread as parked in a blocking wait until dropped.
pub struct BlockedGuard {
    counted: bool,
}

impl Drop for BlockedGuard {
    fn drop(&mut self) {
        if self.counted {
            ACTIVE.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// Threads parked in a wait that **only another task can end** — `recv`,
/// `mutex.lock`, `join`. Unlike `sleep_ms` or a socket read, nothing external
/// (a timer, the network) will ever wake these: progress requires another
/// Spid3r task to act. The deadlock watchdog (see `watchdog_loop`) reads this
/// counter; a park that is merely slow (sleep, I/O) must not touch it.
static UNTIMED_PARKED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// RAII marker for one untimed park; the count is held for the whole wait so
/// the 50 ms wake-ups inside `recv` do not open a sampling gap.
pub struct UntimedPark;

impl Drop for UntimedPark {
    fn drop(&mut self) {
        UNTIMED_PARKED.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Declare that the current thread is about to park in a wait only another
/// task can end. Drop the guard when the wait is over.
pub fn enter_untimed_park() -> UntimedPark {
    UNTIMED_PARKED.fetch_add(1, Ordering::SeqCst);
    UntimedPark
}

/// Threads currently parked in an untimed wait (diagnostic; watchdog).
pub fn untimed_parked() -> usize {
    UNTIMED_PARKED.load(Ordering::SeqCst)
}

/// Identity of the execution context that is running on this thread: the
/// spawned task's state pointer, or `usize::MAX` for the main program. Used by
/// `MutexData` to detect a task re-locking a mutex it already holds.
pub(crate) fn current_owner_id() -> usize {
    CURRENT_TASK.with(|c| match c.borrow().as_ref() {
        Some(state) => Arc::as_ptr(state) as usize,
        None => usize::MAX,
    })
}

/// Declare that the current thread is about to park in a blocking wait
/// (`recv`/`join`/`mutex.lock`/`sleep`). While parked it cannot mutate values,
/// so it must not hold the collector back. The matching [`BlockedGuard`]
/// re-counts the thread when the wait returns. A no-op on a thread that never
/// entered through an engine entry point (never underflows).
pub fn enter_blocked() -> BlockedGuard {
    // A compare-exchange loop rather than `fetch_update`/`try_update`: the
    // latter was renamed in Rust 1.99 (deprecated old name), and this crate
    // must build without deprecation warnings on both sides of that change.
    let mut current = ACTIVE.load(Ordering::SeqCst);
    loop {
        let Some(next) = current.checked_sub(1) else {
            return BlockedGuard { counted: false };
        };
        match ACTIVE.compare_exchange_weak(current, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return BlockedGuard { counted: true },
            Err(observed) => current = observed,
        }
    }
}

/// Number of threads currently executing Spid3r code (see [`ACTIVE`]).
pub fn active_threads() -> usize {
    ACTIVE.load(Ordering::SeqCst)
}

/// Is a mark-sweep safe right now? True when this thread is the only one
/// executing Spid3r code, so no other thread can be allocating, cloning, or
/// moving a tracked value while the collector reads strong counts.
pub fn gc_quiescent() -> bool {
    active_threads() == 1
}

thread_local! {
    /// The cancellation state of the task running on this thread, if any.
    /// The engines poll `current_task_cancelled()` at their cancellation
    /// points; the main task has none (it is not cancellable).
    static CURRENT_TASK: std::cell::RefCell<Option<Arc<TaskState>>> =
        const { std::cell::RefCell::new(None) };
    /// The user-thrown value behind the most recent String error on this
    /// thread. Set at seams that only carry `String` errors (callback hosts,
    /// `task.join()`), consumed by the engine's method-call seam so `catch`
    /// rebinds the original value (audit A-9).
    static LAST_ERROR_THROWN: std::cell::Cell<Option<Value>> =
        const { std::cell::Cell::new(None) };
}

/// Install the cancellation state for the task running on this thread.
pub(crate) fn set_current_task(state: Option<Arc<TaskState>>) {
    CURRENT_TASK.with(|c| *c.borrow_mut() = state);
}

/// Has the task running on this thread been cancelled? False for the main
/// task and for any thread that is not running a spawned task.
pub fn current_task_cancelled() -> bool {
    CURRENT_TASK.with(|c| {
        c.borrow()
            .as_ref()
            .map(|s| s.cancelled.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap_or(false)
    })
}

/// Stash the thrown value behind the String error the current operation is
/// about to return (no-op when the error is not a user throw).
pub fn stash_error_thrown(v: Option<Value>) {
    if let Some(v) = v {
        LAST_ERROR_THROWN.with(|c| c.set(Some(v)));
    }
}

/// Take the stashed thrown value, if any, at the engine's error seam.
pub fn take_error_thrown() -> Option<Value> {
    LAST_ERROR_THROWN.with(|c| c.take())
}

/// Drop any stashed thrown value. Called when method dispatch starts, so a
/// value stashed by an operation whose `String` error was handled elsewhere
/// (and therefore never consumed by an engine seam) cannot attach itself to an
/// unrelated later error on the same thread.
pub fn clear_error_thrown() {
    LAST_ERROR_THROWN.with(|c| c.set(None));
}

/// Unbounded message queue between tasks.
pub struct ChannelData {
    queue: Mutex<VecDeque<Value>>,
    cv: Condvar,
}

impl ChannelData {
    pub fn new() -> Self {
        ChannelData {
            queue: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
        }
    }

    pub fn send(&self, v: Value) {
        self.queue.lock().unwrap().push_back(v);
        self.cv.notify_one();
    }

    /// Blocks until a message arrives. Parks the worker; it does not spin.
    /// While parked the thread is reported as blocked, so a concurrent
    /// `gc_collect()` on another thread is allowed (it cannot be mutating).
    pub fn recv(&self) -> Value {
        self.recv_checked().unwrap_or(Value::Null)
    }

    /// Receive a value, but give up when the running task is cancelled.
    /// The wait is time-sliced so a cancelled task wakes up promptly even
    /// while it is parked on an empty channel.
    pub fn recv_checked(&self) -> Result<Value, String> {
        let mut q = self.queue.lock().unwrap();
        if q.is_empty() {
            // Nothing can ever arrive without another task to send it, so the
            // wait is untimed: register it and make sure the deadlock
            // watchdog is running (a program that never spawns still has one).
            ensure_deadlock_watchdog();
            let _untimed = enter_untimed_park();
            while q.is_empty() {
                if current_task_cancelled() {
                    return Err("task cancelled".to_string());
                }
                let _blocked = enter_blocked();
                let (guard, _timeout) = self
                    .cv
                    .wait_timeout(q, std::time::Duration::from_millis(50))
                    .unwrap();
                q = guard;
            }
        }
        Ok(q.pop_front().unwrap())
    }

    pub fn try_recv(&self) -> Option<Value> {
        self.queue.lock().unwrap().pop_front()
    }

    pub fn len(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.lock().unwrap().is_empty()
    }
}

impl Default for ChannelData {
    fn default() -> Self {
        Self::new()
    }
}

/// Mutual exclusion around a value. `lock` returns the inner value; the lock
/// is released when the interpreter unlocks the guard at the end of the scope.
/// `MutexData` is shared as `Arc<MutexData>` (no outer wrapper mutex): `lock`
/// parks on the internal condvar while holding no other guard, so concurrent
/// `lock`/`unlock` from several tasks can never deadlock on dispatch.
///
/// The lock is **not reentrant**: a task that locks a mutex it already holds
/// can never be released by anyone else, so `lock` detects that case from the
/// recorded owner and returns a catchable error instead of parking forever
/// (which used to hang the whole program with no diagnostic).
pub struct MutexData {
    state: Mutex<LockState>,
    cv: Condvar,
    value: Mutex<Value>,
}

/// Lock flag plus the identity of the holding execution context
/// (`current_owner_id`: the task's state pointer, or the main-program
/// sentinel). One mutex keeps the flag and the owner consistent.
struct LockState {
    locked: bool,
    owner: Option<usize>,
}

/// Message for a task re-locking a mutex it already holds.
pub const SELF_DEADLOCK_MSG: &str =
    "mutex: deadlock - this task already holds the lock (mutexes are not reentrant)";

impl MutexData {
    pub fn new(v: Value) -> Self {
        MutexData {
            state: Mutex::new(LockState {
                locked: false,
                owner: None,
            }),
            cv: Condvar::new(),
            value: Mutex::new(v),
        }
    }

    /// Takes the lock and returns the inner value (aliased, since collections
    /// are reference types). The wait is reported as blocked, so it does not
    /// hold a concurrent `gc_collect()` back.
    ///
    /// # Errors
    /// The calling context already holds this mutex — a guaranteed self
    /// deadlock, reported instead of parked on.
    pub fn lock(&self) -> Result<Value, String> {
        let me = current_owner_id();
        let mut state = self.state.lock().unwrap();
        if state.locked && state.owner == Some(me) {
            return Err(SELF_DEADLOCK_MSG.to_string());
        }
        if state.locked {
            ensure_deadlock_watchdog();
            let _untimed = enter_untimed_park();
            while state.locked {
                let _blocked = enter_blocked();
                state = self.cv.wait(state).unwrap();
            }
        }
        state.locked = true;
        state.owner = Some(me);
        drop(state);
        Ok(self.value.lock().unwrap().clone())
    }

    pub fn unlock(&self) {
        let mut state = self.state.lock().unwrap();
        state.locked = false;
        state.owner = None;
        drop(state);
        self.cv.notify_one();
    }
}

/// A held user mutex that unlocks when dropped — including during a panic
/// unwind. The engines record one of these per `mu.lock()`; without it, a
/// panic unwinding through the engine leaves the mutex flagged as locked
/// forever, deadlocking every later `lock()` in a long-lived embedding host.
pub struct MutexGuardToken {
    m: Option<Arc<MutexData>>,
}

impl MutexGuardToken {
    pub fn new(m: Arc<MutexData>) -> Self {
        MutexGuardToken { m: Some(m) }
    }

    /// The held mutex, if the token is still armed.
    pub fn mutex(&self) -> Option<&Arc<MutexData>> {
        self.m.as_ref()
    }

    /// Mark the token as released: the mutex was unlocked explicitly, so the
    /// token must not unlock it again on drop (a second unlock could free a
    /// lock another task legitimately holds).
    pub fn disarm(&mut self) {
        self.m = None;
    }
}

impl Drop for MutexGuardToken {
    fn drop(&mut self) {
        if let Some(m) = self.m.take() {
            m.unlock();
        }
    }
}

/// A running or finished spawned task.
pub struct TaskHandle {
    pub id: usize,
    state: Arc<TaskState>,
}

pub(crate) struct TaskState {
    result: Mutex<Option<TaskOutcome>>,
    cv: Condvar,
    /// Set by `task_cancel`; the running task observes it at its next
    /// cancellation point (instruction/statement boundary in the tree and VM
    /// engines, runtime-call boundary in the native one).
    cancelled: std::sync::atomic::AtomicBool,
}

impl TaskHandle {
    pub fn is_done(&self) -> bool {
        self.state.result.lock().unwrap().is_some()
    }

    /// Request cancellation. The task stops at its next cancellation point
    /// and its result becomes an error ("task cancelled"); joining a
    /// cancelled task returns that error rather than a value. Cancelling a
    /// finished task is a no-op.
    pub fn cancel(&self) -> bool {
        if self.is_done() {
            return false;
        }
        self.state
            .cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Wake a joiner so it does not wait for the task to notice.
        self.state.cv.notify_all();
        true
    }

    /// Was this task cancelled?
    pub fn is_cancelled(&self) -> bool {
        self.state
            .cancelled
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Task state for `task.status()`: `"running"`, `"done"`, or `"failed"`.
    /// Peeks at the result without consuming it.
    pub fn status(&self) -> &'static str {
        if self.is_cancelled() {
            return "cancelled";
        }
        match self.state.result.lock().unwrap().as_ref() {
            None => "running",
            Some(o) => match &o.result {
                Ok(_) => "done",
                Err(_) => "failed",
            },
        }
    }

    /// Blocks until the task finishes and returns its result. Repeatable:
    /// the result is cloned rather than taken, so joining twice returns the
    /// same value instead of blocking forever on the emptied slot. The wait is
    /// reported as blocked (see [`enter_blocked`]).
    pub fn join(&self) -> Result<Value, String> {
        let mut r = self.state.result.lock().unwrap();
        if r.is_none() {
            // The joined task is the only thing that can end this wait, so it
            // counts as an untimed park for the deadlock watchdog.
            let _untimed = enter_untimed_park();
            while r.is_none() {
                let _blocked = enter_blocked();
                r = self.state.cv.wait(r).unwrap();
            }
        }
        r.as_ref().unwrap().result.clone()
    }

    /// The user value the task threw, when it failed by throwing. `join`
    /// surfaces only the error string; this restores the structured payload
    /// for `catch` on the joining side (audit A-9).
    pub fn thrown_value(&self) -> Option<Value> {
        self.state
            .result
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|o| o.thrown.clone())
    }
}

type Job = Box<dyn FnOnce() -> TaskOutcome + Send + 'static>;

/// Account a finished task and wake a thread waiting in [`Scheduler::drain`].
/// `saturating_sub` keeps a double decrement (which would wrap `usize` and
/// turn into a huge bogus live count) from corrupting the accounting.
fn task_finished(inner: &Arc<SchedInner>) {
    let mut n = inner.live.lock().unwrap();
    *n = n.saturating_sub(1);
    #[cfg(not(target_arch = "wasm32"))]
    if *n == 0 {
        inner.idle.notify_all();
    }
}

struct SchedInner {
    #[cfg(not(target_arch = "wasm32"))]
    queue: Mutex<VecDeque<Job>>,
    /// Signalled when a job is queued.
    #[cfg(not(target_arch = "wasm32"))]
    cv: Condvar,
    /// Signalled when a worker takes a job, waking a producer parked on a full
    /// queue (bounded-queue backpressure).
    #[cfg(not(target_arch = "wasm32"))]
    space_cv: Condvar,
    /// Maximum queued jobs; producers park when it is reached.
    #[cfg(not(target_arch = "wasm32"))]
    queue_cap: usize,
    /// Worker accounting for grow-on-demand scheduling (audit A-3): `alive`
    /// worker threads exist, `busy` of them are inside a job (possibly
    /// blocked on recv/join/mutex/sleep). Lock order: `queue` before
    /// `workers`.
    #[cfg(not(target_arch = "wasm32"))]
    workers: Mutex<Workers>,
    /// Base pool size (available parallelism); workers above it retire.
    #[cfg(not(target_arch = "wasm32"))]
    base: usize,
    /// Ceiling for grow-on-demand workers (`SPID3R_MAX_WORKERS`, default 256).
    #[cfg(not(target_arch = "wasm32"))]
    max_workers: usize,
    /// Join handles of every worker thread started, so `shutdown` can join
    /// them (finished ones are pruned as new workers start).
    #[cfg(not(target_arch = "wasm32"))]
    handles: Mutex<Vec<JoinHandle<()>>>,
    /// Set by `shutdown`: workers stop taking jobs and exit.
    stopping: AtomicBool,
    /// Watchdog ticker: signalled by `shutdown` to end the watchdog promptly.
    #[cfg(not(target_arch = "wasm32"))]
    watch: Mutex<()>,
    #[cfg(not(target_arch = "wasm32"))]
    watch_cv: Condvar,
    next_id: Mutex<usize>,
    live: Mutex<usize>,
    /// Signalled whenever `live` reaches zero, so program exit can wait for
    /// outstanding tasks without polling (audit A-3: join on exit).
    #[cfg(not(target_arch = "wasm32"))]
    idle: Condvar,
}

#[cfg(not(target_arch = "wasm32"))]
struct Workers {
    alive: usize,
    busy: usize,
}

/// Default ceiling for grow-on-demand workers. Beyond this many concurrently
/// blocked tasks the pool cannot add threads (see `watchdog_loop` for the
/// stall report); `SPID3R_MAX_WORKERS` overrides it.
#[cfg(not(target_arch = "wasm32"))]
const DEFAULT_MAX_WORKERS: usize = 256;

/// Default bound on queued jobs. A spawn storm parks the producer instead of
/// growing memory without limit; `SPID3R_QUEUE_CAP` overrides it.
#[cfg(not(target_arch = "wasm32"))]
const DEFAULT_QUEUE_CAP: usize = 65536;

/// How long an idle worker above the base pool waits before retiring.
#[cfg(not(target_arch = "wasm32"))]
const RETIRE_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

/// Watchdog sampling period and how many consecutive stalled samples count as
/// a stall worth reporting (~3 s).
#[cfg(not(target_arch = "wasm32"))]
const WATCHDOG_TICK: std::time::Duration = std::time::Duration::from_secs(1);
#[cfg(not(target_arch = "wasm32"))]
const WATCHDOG_STALL_TICKS: u32 = 3;

/// Start the process-wide pool (and with it the deadlock watchdog) the first
/// time a task parks in a wait only another task can end. A program that
/// never spawns has no pool of its own, so without this its single
/// `recv`-with-no-sender would hang with no watchdog to notice.
#[cfg(not(target_arch = "wasm32"))]
fn ensure_deadlock_watchdog() {
    static ENSURE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ENSURE.get_or_init(|| {
        Scheduler::shared();
    });
}

#[cfg(target_arch = "wasm32")]
fn ensure_deadlock_watchdog() {}

/// Worker pool. One instance is shared by the whole process (see
/// [`Scheduler::shared`]); tests can build an isolated one.
pub struct Scheduler {
    inner: Arc<SchedInner>,
}

static SCHED: OnceLock<Mutex<Option<Arc<Scheduler>>>> = OnceLock::new();

/// Stack size for each worker thread: large enough that a task running
/// deep Spid3r recursion is stopped by the interpreter's call-depth guard
/// rather than by the OS stack.
#[cfg(not(target_arch = "wasm32"))]
const WORKER_STACK_BYTES: usize = 32 * 1024 * 1024;

impl Scheduler {
    /// The process-wide pool, started on first use. After
    /// [`Scheduler::shutdown`] the singleton is restarted on the next call, so
    /// a REPL or embedding host can keep spawning tasks after a clean stop.
    pub fn shared() -> Arc<Scheduler> {
        let slot = SCHED.get_or_init(|| Mutex::new(None));
        let mut guard = slot.lock().unwrap();
        if let Some(sched) = guard.as_ref() {
            if !sched.is_stopped() {
                return sched.clone();
            }
        }
        let sched = Scheduler::start();
        *guard = Some(sched.clone());
        sched
    }

    /// The running process-wide pool, if one exists and has not been shut
    /// down. Unlike [`Scheduler::shared`] this never starts a pool — used by
    /// the exit path, which must not spin threads up just to stop them.
    pub fn current() -> Option<Arc<Scheduler>> {
        let slot = SCHED.get_or_init(|| Mutex::new(None));
        let guard = slot.lock().unwrap();
        guard.as_ref().filter(|sched| !sched.is_stopped()).cloned()
    }

    fn start() -> Arc<Scheduler> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let max_workers = env_usize("SPID3R_MAX_WORKERS", DEFAULT_MAX_WORKERS).max(1);
            let queue_cap = env_usize("SPID3R_QUEUE_CAP", DEFAULT_QUEUE_CAP).max(1);
            let base = thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                .min(max_workers);
            Scheduler::build(base, queue_cap, max_workers)
        }
        #[cfg(target_arch = "wasm32")]
        {
            Scheduler::build()
        }
    }

    /// Build an isolated pool (not the process-wide singleton) with an explicit
    /// worker count and queue bound. Primarily for tests that need
    /// deterministic accounting; production code uses [`Scheduler::shared`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn isolated(workers: usize, queue_cap: usize) -> Arc<Scheduler> {
        let workers = workers.max(1);
        Scheduler::build(workers, queue_cap.max(1), workers)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn build(base: usize, queue_cap: usize, max_workers: usize) -> Arc<Scheduler> {
        let inner = Arc::new(SchedInner {
            queue: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            space_cv: Condvar::new(),
            queue_cap,
            workers: Mutex::new(Workers { alive: 0, busy: 0 }),
            base,
            max_workers,
            handles: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
            watch: Mutex::new(()),
            watch_cv: Condvar::new(),
            next_id: Mutex::new(1),
            live: Mutex::new(0),
            idle: Condvar::new(),
        });
        for _ in 0..base {
            Scheduler::spawn_worker(&inner, base);
        }
        Scheduler::spawn_watchdog(&inner);
        Arc::new(Scheduler { inner })
    }

    #[cfg(target_arch = "wasm32")]
    fn build() -> Arc<Scheduler> {
        Arc::new(Scheduler {
            inner: Arc::new(SchedInner {
                stopping: AtomicBool::new(false),
                next_id: Mutex::new(1),
                live: Mutex::new(0),
            }),
        })
    }

    /// Start one worker thread and account it in `workers.alive`. The cap is
    /// enforced under the accounting lock so concurrent spawns (several tasks
    /// spawning at once, each computing the same "missing" count from a stale
    /// snapshot) cannot overshoot the ceiling.
    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_worker(inner: &Arc<SchedInner>, base: usize) {
        {
            let mut w = inner.workers.lock().unwrap();
            if w.alive >= inner.max_workers {
                return;
            }
            w.alive += 1;
        }
        let loop_inner = inner.clone();
        let spawned = thread::Builder::new()
            .name("spid3r-worker".to_string())
            .stack_size(WORKER_STACK_BYTES)
            .spawn(move || worker_loop(loop_inner, base));
        match spawned {
            Ok(handle) => {
                let mut handles = inner.handles.lock().unwrap();
                // Prune handles of workers that already retired, so a long
                // run with growth/retire churn does not accumulate them.
                if handles.len() > 64 {
                    handles.retain(|h| !h.is_finished());
                }
                handles.push(handle);
            }
            Err(_) => {
                // The thread could not be created: undo the reservation so the
                // accounting stays exact (and the next spawn can retry).
                inner.workers.lock().unwrap().alive -= 1;
            }
        }
    }

    /// Start the stall watchdog (audit A-3): at the worker ceiling, a queued
    /// job whose workers are all blocked can never run. Rather than hang
    /// silently, report it once with an actionable message.
    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_watchdog(inner: &Arc<SchedInner>) {
        let loop_inner = inner.clone();
        let _ = thread::Builder::new()
            .name("spid3r-watchdog".to_string())
            .stack_size(256 * 1024)
            .spawn(move || watchdog_loop(loop_inner));
    }

    pub fn pool_size(&self) -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        {
            thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        }
        #[cfg(target_arch = "wasm32")]
        {
            1
        }
    }

    /// Queue a job. Returns the task handle; the caller can `join` it.
    pub fn spawn_job(&self, job: Job) -> Arc<TaskHandle> {
        let id = {
            let mut n = self.inner.next_id.lock().unwrap();
            let id = *n;
            *n += 1;
            id
        };
        let state = Arc::new(TaskState {
            result: Mutex::new(None),
            cv: Condvar::new(),
            cancelled: std::sync::atomic::AtomicBool::new(false),
        });
        let handle = Arc::new(TaskHandle {
            id,
            state: state.clone(),
        });
        *self.inner.live.lock().unwrap() += 1;

        #[cfg(target_arch = "wasm32")]
        {
            set_current_task(Some(state.clone()));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
            set_current_task(None);
            let stored: TaskOutcome = match result {
                Ok(r) => r,
                Err(_) => TaskOutcome {
                    result: Err("task panicked".to_string()),
                    thrown: None,
                },
            };
            {
                let mut r = state.result.lock().unwrap();
                *r = Some(stored);
            }
            task_finished(&self.inner);
            handle
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            // A stopped pool has no workers: queueing would leave `join()`
            // waiting forever. Degrade to running the job on this thread —
            // never lose the work, never hang the caller.
            if self.inner.stopping.load(Ordering::SeqCst) {
                set_current_task(Some(state.clone()));
                let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                set_current_task(None);
                let stored: TaskOutcome = match out {
                    Ok(o) => o,
                    Err(_) => TaskOutcome {
                        result: Err("task panicked".to_string()),
                        thrown: None,
                    },
                };
                {
                    let mut r = state.result.lock().unwrap();
                    *r = Some(stored);
                    state.cv.notify_all();
                }
                task_finished(&self.inner);
                return handle;
            }
            let inner = self.inner.clone();
            let wrapped: Job = Box::new(move || {
                // Count this thread as executing Spid3r code while the job
                // runs, so a concurrent `gc_collect()` on another thread sees
                // it and refuses (see `gc_quiescent`).
                let _active = enter_active();
                set_current_task(Some(state.clone()));
                let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                set_current_task(None);
                let stored: TaskOutcome = match out {
                    Ok(o) => o,
                    Err(_) => TaskOutcome {
                        result: Err("task panicked".to_string()),
                        thrown: None,
                    },
                };
                {
                    let mut r = state.result.lock().unwrap();
                    *r = Some(stored);
                    state.cv.notify_all();
                }
                // Decrement `live` and wake anyone draining at program exit.
                task_finished(&inner);
                TaskOutcome {
                    result: Ok(Value::Null),
                    thrown: None,
                }
            });
            self.enqueue(wrapped);
            self.grow_for_queue();
            handle
        }
    }

    /// Queue a job, applying backpressure when the queue is at its bound
    /// (audit A-3: a spawn storm must not grow memory without limit). The
    /// producer grows the pool first — the queued jobs may all be blocked, in
    /// which case extra workers are the only way to drain — and then parks
    /// until a worker takes a job. Workers signal `space_cv` while holding the
    /// queue lock, so a producer cannot miss the wakeup.
    #[cfg(not(target_arch = "wasm32"))]
    fn enqueue(&self, job: Job) {
        let mut job = Some(job);
        {
            let mut q = self.inner.queue.lock().unwrap();
            if q.len() < self.inner.queue_cap {
                q.push_back(job.take().expect("job"));
                drop(q);
                self.inner.cv.notify_one();
                return;
            }
        }
        self.grow_for_queue();
        let mut q = self.inner.queue.lock().unwrap();
        while q.len() >= self.inner.queue_cap {
            q = self
                .inner
                .space_cv
                .wait(q)
                .unwrap_or_else(|e| e.into_inner());
        }
        q.push_back(job.take().expect("job"));
        drop(q);
        self.inner.cv.notify_one();
    }

    /// Grow the pool so every outstanding job has a worker (audit A-3). A
    /// worker inside a job may block for an unbounded time
    /// (recv/join/mutex/sleep), so a queued job can starve behind blocked ones.
    /// The invariant that guarantees progress is `alive >= busy + queued`,
    /// because every queued job might block once it starts. Adding one worker
    /// per spawn is not enough: with 20 workers all blocked and 7 jobs queued,
    /// that extra worker just picks up another blocking job and leaves the last
    /// one queued forever.
    ///
    /// Lock order is `queue` before `workers` (see SchedInner), so the queue
    /// length is read first and its lock released.
    #[cfg(not(target_arch = "wasm32"))]
    fn grow_for_queue(&self) {
        let queued = self.inner.queue.lock().unwrap().len();
        let (busy, alive) = {
            let w = self.inner.workers.lock().unwrap();
            (w.busy, w.alive)
        };
        let available = alive.saturating_sub(busy);
        if queued > available {
            let needed = busy.saturating_add(queued);
            let missing = needed.saturating_sub(alive);
            let room = self.inner.max_workers.saturating_sub(alive);
            for _ in 0..missing.min(room) {
                Scheduler::spawn_worker(&self.inner, self.inner.base);
            }
        }
    }

    pub fn live_tasks(&self) -> usize {
        *self.inner.live.lock().unwrap()
    }

    /// Jobs waiting in the ready queue (diagnostic; used by tests).
    pub fn queued_jobs(&self) -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.queue.lock().unwrap().len()
        }
        #[cfg(target_arch = "wasm32")]
        {
            0
        }
    }

    /// Worker threads currently alive (diagnostic; used by tests).
    pub fn worker_count(&self) -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.workers.lock().unwrap().alive
        }
        #[cfg(target_arch = "wasm32")]
        {
            1
        }
    }

    /// Configured ceiling for grow-on-demand workers.
    pub fn max_workers(&self) -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.max_workers
        }
        #[cfg(target_arch = "wasm32")]
        {
            1
        }
    }

    /// Configured bound on queued jobs.
    pub fn queue_cap(&self) -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.queue_cap
        }
        #[cfg(target_arch = "wasm32")]
        {
            0
        }
    }

    /// Has [`Scheduler::shutdown`] run on this pool?
    pub fn is_stopped(&self) -> bool {
        self.inner.stopping.load(Ordering::SeqCst)
    }

    /// Wait until no spawned task is running, or until `timeout` elapses.
    /// Returns the number of tasks still running when the wait ended (0 on a
    /// clean drain). Parks on a condvar, so it does not spin.
    ///
    /// Used at program exit (audit A-3): a program whose `main` returns while
    /// tasks are still running used to lose their output to process teardown.
    pub fn drain(&self, timeout: std::time::Duration) -> usize {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = timeout;
            self.live_tasks()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let deadline = std::time::Instant::now() + timeout;
            let mut n = self.inner.live.lock().unwrap();
            loop {
                if *n == 0 {
                    return 0;
                }
                let now = std::time::Instant::now();
                if now >= deadline {
                    return *n;
                }
                let (guard, _) = self
                    .inner
                    .idle
                    .wait_timeout(n, deadline - now)
                    .unwrap_or_else(|e| e.into_inner());
                n = guard;
            }
        }
    }

    /// Stop the pool and join its worker threads (audit A-3: deterministic
    /// process teardown — no thread is killed mid-write at exit).
    ///
    /// Idempotent. Returns `true` when every worker was joined, `false` when
    /// tasks were still running (a stuck task's thread cannot be joined without
    /// blocking forever, so it is left to the OS as the process exits). The
    /// caller must drain first — the CLI calls `join_tasks_at_exit` and then
    /// this. After a shutdown, [`Scheduler::shared`] starts a fresh pool.
    pub fn shutdown(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            self.inner.stopping.store(true, Ordering::SeqCst);
            true
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let already = self.inner.stopping.swap(true, Ordering::SeqCst);
            // Wake producers, idle workers, drainers and the watchdog.
            self.inner.cv.notify_all();
            self.inner.space_cv.notify_all();
            {
                let _tick = self.inner.watch.lock().unwrap();
                self.inner.watch_cv.notify_all();
            }
            // Never join a worker that is still inside a job: it may be parked
            // in recv/join/mutex forever, and joining would hang the exit path.
            // (Checked on every call, including a repeat after a failed one.)
            if self.live_tasks() > 0 {
                return false;
            }
            if already {
                return true;
            }
            let handles: Vec<JoinHandle<()>> = {
                let mut h = self.inner.handles.lock().unwrap();
                std::mem::take(&mut *h)
            };
            for h in handles {
                let _ = h.join();
            }
            true
        }
    }
}

/// How long program exit waits for outstanding tasks by default. A program
/// whose tasks are blocked forever still terminates (with a warning) instead
/// of hanging the host; `SPID3R_TASK_DRAIN_MS` overrides it (`0` disables the
/// wait and restores the old, output-losing teardown).
pub const DEFAULT_DRAIN_MS: u64 = 2000;

/// Configured drain deadline in milliseconds (`SPID3R_TASK_DRAIN_MS`, else
/// [`DEFAULT_DRAIN_MS`]).
fn drain_ms() -> u64 {
    std::env::var("SPID3R_TASK_DRAIN_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_DRAIN_MS)
}

/// Wait for outstanding tasks at program exit; returns the number still
/// running after the deadline (0 when everything finished). Never starts a
/// pool: if nothing ever spawned, there is nothing to wait for.
pub fn drain_at_exit() -> usize {
    let ms = drain_ms();
    match Scheduler::current() {
        Some(sched) => {
            if ms == 0 {
                sched.live_tasks()
            } else {
                sched.drain(std::time::Duration::from_millis(ms))
            }
        }
        None => 0,
    }
}

/// Stop the process-wide pool and join its workers. Called once by the CLI
/// after the last program run, so teardown is deterministic (no worker is
/// killed while writing output). Safe to call when no pool was ever started.
pub fn shutdown() {
    if let Some(sched) = Scheduler::current() {
        sched.shutdown();
    }
}

/// Program-exit hook shared by both engines: drain outstanding tasks and, when
/// some are still running after the deadline, say so on stderr instead of
/// silently discarding their work.
pub fn join_tasks_at_exit() -> usize {
    let ms = drain_ms();
    let left = drain_at_exit();
    if left > 0 {
        eprintln!(
            "spid3r: warning: {} task(s) still running after main returned \
             (waited {} ms; set SPID3R_TASK_DRAIN_MS to change)",
            left, ms
        );
    }
    left
}

/// Worker body (audit A-3): take jobs, tracking `busy`; when the queue stays
/// empty past `RETIRE_AFTER` and the pool is above its base size, retire so
/// bursts of blocked tasks grow the pool and idle growth shrinks back.
/// Returns when [`Scheduler::shutdown`] sets the stopping flag.
#[cfg(not(target_arch = "wasm32"))]
fn worker_loop(inner: Arc<SchedInner>, base: usize) {
    loop {
        let job = {
            let mut q = inner.queue.lock().unwrap();
            loop {
                if inner.stopping.load(Ordering::SeqCst) {
                    inner.workers.lock().unwrap().alive -= 1;
                    return;
                }
                if let Some(j) = q.pop_front() {
                    // Wake a producer parked on a full queue. Notifying while
                    // holding the queue lock closes the lost-wakeup window:
                    // the producer re-checks `len` under the same lock.
                    inner.space_cv.notify_one();
                    break Some(j);
                }
                let (guard, timeout) = inner.cv.wait_timeout(q, RETIRE_AFTER).unwrap();
                q = guard;
                if timeout.timed_out() && q.is_empty() {
                    if inner.stopping.load(Ordering::SeqCst) {
                        inner.workers.lock().unwrap().alive -= 1;
                        return;
                    }
                    let mut w = inner.workers.lock().unwrap();
                    if w.alive > base {
                        w.alive -= 1;
                        break None;
                    }
                }
            }
        };
        match job {
            Some(job) => {
                inner.workers.lock().unwrap().busy += 1;
                let _ = job();
                inner.workers.lock().unwrap().busy -= 1;
            }
            None => return,
        }
    }
}

/// Stall watchdog (audit A-3). At the worker ceiling, a queued job whose
/// workers are all parked (recv/join/mutex/sleep) can never run: without this
/// report the program would hang with no diagnostic. Samples once per second
/// and reports a stall after `WATCHDOG_STALL_TICKS` consecutive samples where
/// work is queued, every worker is busy, and the pool is at its ceiling.
///
/// It is also the **deadlock detector**: when nothing is queued, no worker is
/// inside a job, no task is live, and some thread is parked in a wait only
/// another task could end (recv/mutex/join — see [`untimed_parked`]), no
/// future event can unblock it. After the same sustained window the process
/// aborts with a diagnostic instead of hanging silently (the same policy as
/// Go's "all goroutines are asleep" fatal error). `SPID3R_DEADLOCK_ABORT=0`
/// downgrades the abort to a warning for embedding hosts that prefer to
/// supervise the wait themselves. Socket waits (`tcp_accept`, `tcp_read`) and
/// `sleep_ms` never register as untimed parks, so an idle server or a long
/// sleep is not mistaken for a deadlock.
#[cfg(not(target_arch = "wasm32"))]
fn watchdog_loop(inner: Arc<SchedInner>) {
    let mut stalled: u32 = 0;
    let mut deadlocked: u32 = 0;
    let mut deadlock_warned = false;
    loop {
        {
            let tick = inner.watch.lock().unwrap();
            let (tick, _) = inner
                .watch_cv
                .wait_timeout(tick, WATCHDOG_TICK)
                .unwrap_or_else(|e| e.into_inner());
            drop(tick);
        }
        if inner.stopping.load(Ordering::SeqCst) {
            return;
        }
        let queued = inner.queue.lock().unwrap().len();
        let (busy, alive) = {
            let w = inner.workers.lock().unwrap();
            (w.busy, w.alive)
        };
        let at_ceiling = alive >= inner.max_workers;
        if queued > 0 && alive > 0 && busy >= alive && at_ceiling {
            stalled += 1;
            if stalled == WATCHDOG_STALL_TICKS {
                eprintln!(
                    "spid3r: warning: {} task(s) queued but all {} workers are blocked; \
                     queued work cannot run. Join or release the blocked tasks, or raise \
                     SPID3R_MAX_WORKERS (currently {}).",
                    queued, alive, inner.max_workers
                );
            }
        } else {
            stalled = 0;
        }
        // Deadlock: the only live context is a park that no task can end.
        // `busy == 0 && live == 0 && queued == 0` means no task exists that
        // could send, unlock, or finish; `untimed > 0` means someone is
        // waiting for exactly that. A worker parked untimed is `busy`, so
        // this can only be the main program (or an embedder thread) — which
        // is precisely the hang with no possible waker.
        let untimed = untimed_parked();
        let live = *inner.live.lock().unwrap();
        if queued == 0 && busy == 0 && live == 0 && untimed > 0 {
            deadlocked += 1;
            if deadlocked == WATCHDOG_STALL_TICKS {
                let msg = format!(
                    "spid3r: fatal: deadlock - {} thread(s) blocked in a channel/mutex/join \
                     wait and no task is running, queued, or able to run; nothing can ever \
                     wake them",
                    untimed
                );
                if deadlock_abort_enabled() {
                    eprintln!("{}", msg);
                    std::process::exit(1);
                }
                if !deadlock_warned {
                    eprintln!("{} (SPID3R_DEADLOCK_ABORT=0: continuing to wait)", msg);
                    deadlock_warned = true;
                }
                deadlocked = 0;
            }
        } else {
            deadlocked = 0;
        }
    }
}

/// Should a detected deadlock abort the process? Default yes; an embedding
/// host that supervises its own waits sets `SPID3R_DEADLOCK_ABORT=0`.
#[cfg(not(target_arch = "wasm32"))]
fn deadlock_abort_enabled() -> bool {
    std::env::var("SPID3R_DEADLOCK_ABORT")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(true)
}

/// Read a positive `usize` from the environment, falling back to `default`.
#[cfg(not(target_arch = "wasm32"))]
fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// Sleep for `ms` milliseconds. A Spid3r-level call, not a shell command.
pub fn sleep_ms(ms: i64) {
    if ms > 0 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            // A sleeping thread cannot mutate values, so it must not hold a
            // concurrent `gc_collect()` back (see `gc_quiescent`).
            let _blocked = enter_blocked();
            thread::sleep(std::time::Duration::from_millis(ms as u64));
        }
    }
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}
