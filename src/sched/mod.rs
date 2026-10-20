//! The task scheduler. Tasks run on a pool of OS threads (default size:
//! available parallelism) over one shared ready queue. There is no global
//! interpreter lock: two CPU-bound tasks run on two OS threads at once.
//!
//! Documented limits:
//! - `recv` on a channel with no future sender blocks the calling task
//!   forever (it parks on a condvar, so it does not spin). While tasks are
//!   parked the pool grows on demand (up to `MAX_WORKERS`), so blocked tasks
//!   no longer starve queued work; more than `MAX_WORKERS` concurrently
//!   blocked tasks can still stall.
//! - channels are unbounded in v1; growth is limited by process memory.
//! - `await` on a task joins it. Idle workers above the base pool retire
//!   after `RETIRE_AFTER`.
//! - program exit waits up to `DEFAULT_DRAIN_MS` (`SPID3R_TASK_DRAIN_MS`) for
//!   tasks that `main` left running, so their output is not lost to process
//!   teardown; tasks still running after the deadline are reported and the
//!   process exits anyway.
//! - a task that fails while holding a mutex releases it when its scope ends
//!   (locks are scope-based, like `finally`).

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
#[cfg(not(target_arch = "wasm32"))]
use std::thread;

use crate::value::Value;

/// What a spawned job produces: the task's result plus, when the task failed
/// by throwing a user value, the original thrown value — the joining engine
/// rethrows it structurally so `catch` rebinds the value itself, not its
/// display string (audit A-9).
pub struct TaskOutcome {
    pub result: Result<Value, String>,
    pub thrown: Option<Value>,
}

thread_local! {
    /// The user-thrown value behind the most recent String error on this
    /// thread. Set at seams that only carry `String` errors (callback hosts,
    /// `task.join()`), consumed by the engine's method-call seam so `catch`
    /// rebinds the original value (audit A-9).
    static LAST_ERROR_THROWN: std::cell::Cell<Option<Value>> =
        const { std::cell::Cell::new(None) };
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
    pub fn recv(&self) -> Value {
        let mut q = self.queue.lock().unwrap();
        while q.is_empty() {
            q = self.cv.wait(q).unwrap();
        }
        q.pop_front().unwrap()
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
pub struct MutexData {
    locked: Mutex<bool>,
    cv: Condvar,
    value: Mutex<Value>,
}

impl MutexData {
    pub fn new(v: Value) -> Self {
        MutexData {
            locked: Mutex::new(false),
            cv: Condvar::new(),
            value: Mutex::new(v),
        }
    }

    /// Blocks until the lock is free, then returns the inner value (aliased,
    /// since collections are reference types).
    pub fn lock(&self) -> Value {
        let mut locked = self.locked.lock().unwrap();
        while *locked {
            locked = self.cv.wait(locked).unwrap();
        }
        *locked = true;
        drop(locked);
        self.value.lock().unwrap().clone()
    }

    pub fn unlock(&self) {
        let mut locked = self.locked.lock().unwrap();
        *locked = false;
        drop(locked);
        self.cv.notify_one();
    }
}

/// A running or finished spawned task.
pub struct TaskHandle {
    pub id: usize,
    state: Arc<TaskState>,
}

struct TaskState {
    result: Mutex<Option<TaskOutcome>>,
    cv: Condvar,
}

impl TaskHandle {
    pub fn is_done(&self) -> bool {
        self.state.result.lock().unwrap().is_some()
    }

    /// Task state for `task.status()`: `"running"`, `"done"`, or `"failed"`.
    /// Peeks at the result without consuming it.
    pub fn status(&self) -> &'static str {
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
    /// same value instead of blocking forever on the emptied slot.
    pub fn join(&self) -> Result<Value, String> {
        let mut r = self.state.result.lock().unwrap();
        while r.is_none() {
            r = self.state.cv.wait(r).unwrap();
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
    #[cfg(not(target_arch = "wasm32"))]
    cv: Condvar,
    /// Worker accounting for grow-on-demand scheduling (audit A-3): `alive`
    /// worker threads exist, `busy` of them are inside a job (possibly
    /// blocked on recv/join/mutex/sleep). Lock order: `queue` before
    /// `workers`.
    #[cfg(not(target_arch = "wasm32"))]
    workers: Mutex<Workers>,
    /// Base pool size (available parallelism); workers above it retire.
    #[cfg(not(target_arch = "wasm32"))]
    base: usize,
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

/// Upper bound for grow-on-demand workers. Beyond this many concurrently
/// blocked tasks the pool can still stall (documented limit).
#[cfg(not(target_arch = "wasm32"))]
const MAX_WORKERS: usize = 256;

/// How long an idle worker above the base pool waits before retiring.
#[cfg(not(target_arch = "wasm32"))]
const RETIRE_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

/// Worker pool. One instance is shared by the whole process.
pub struct Scheduler {
    inner: Arc<SchedInner>,
}

static SCHED: OnceLock<Arc<Scheduler>> = OnceLock::new();

/// Stack size for each worker thread: large enough that a task running
/// deep Spid3r recursion is stopped by the interpreter's call-depth guard
/// rather than by the OS stack.
#[cfg(not(target_arch = "wasm32"))]
const WORKER_STACK_BYTES: usize = 32 * 1024 * 1024;

impl Scheduler {
    pub fn shared() -> Arc<Scheduler> {
        SCHED.get_or_init(Scheduler::start).clone()
    }

    fn start() -> Arc<Scheduler> {
        #[cfg(not(target_arch = "wasm32"))]
        let base = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(MAX_WORKERS);
        let inner = Arc::new(SchedInner {
            #[cfg(not(target_arch = "wasm32"))]
            queue: Mutex::new(VecDeque::new()),
            #[cfg(not(target_arch = "wasm32"))]
            cv: Condvar::new(),
            #[cfg(not(target_arch = "wasm32"))]
            workers: Mutex::new(Workers {
                alive: 0,
                busy: 0,
            }),
            #[cfg(not(target_arch = "wasm32"))]
            base,
            next_id: Mutex::new(1),
            live: Mutex::new(0),
            #[cfg(not(target_arch = "wasm32"))]
            idle: Condvar::new(),
        });
        #[cfg(not(target_arch = "wasm32"))]
        for _ in 0..base {
            Scheduler::spawn_worker(&inner, base);
        }
        Arc::new(Scheduler { inner })
    }

    /// Start one worker thread and account it in `workers.alive`. The cap is
    /// enforced under the accounting lock so concurrent spawns (several tasks
    /// spawning at once, each computing the same "missing" count from a stale
    /// snapshot) cannot overshoot `MAX_WORKERS`.
    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_worker(inner: &Arc<SchedInner>, base: usize) {
        {
            let mut w = inner.workers.lock().unwrap();
            if w.alive >= MAX_WORKERS {
                return;
            }
            w.alive += 1;
        }
        let loop_inner = inner.clone();
        let spawned = thread::Builder::new()
            .name("spid3r-worker".to_string())
            .stack_size(WORKER_STACK_BYTES)
            .spawn(move || worker_loop(loop_inner, base))
            .is_ok();
        if !spawned {
            // The thread could not be created: undo the reservation so the
            // accounting stays exact (and the next spawn can retry).
            inner.workers.lock().unwrap().alive -= 1;
        }
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
        });
        let handle = Arc::new(TaskHandle {
            id,
            state: state.clone(),
        });
        *self.inner.live.lock().unwrap() += 1;

        #[cfg(target_arch = "wasm32")]
        {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
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
            let inner = self.inner.clone();
            let wrapped: Job = Box::new(move || {
                let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
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
            self.inner.queue.lock().unwrap().push_back(wrapped);
            self.inner.cv.notify_one();
            // Grow on demand (audit A-3). A worker inside a job may block for
            // an unbounded time (recv/join/mutex/sleep), so a queued job can
            // starve behind blocked ones. The invariant that guarantees
            // progress is: at least one worker per outstanding job —
            //     alive >= busy + queued
            // because every queued job might block once it starts. Grow the
            // pool to that size (capped at MAX_WORKERS) instead of adding a
            // single worker per spawn: with 20 workers all blocked and 7 jobs
            // queued, one extra worker would just pick up another blocking
            // job and leave the last one queued forever.
            //
            // Lock order is `queue` before `workers` (see SchedInner), so the
            // queue length is read first and the lock released.
            let queued = self.inner.queue.lock().unwrap().len();
            let (busy, alive) = {
                let w = self.inner.workers.lock().unwrap();
                (w.busy, w.alive)
            };
            let available = alive.saturating_sub(busy);
            if queued > available {
                let needed = busy.saturating_add(queued);
                let missing = needed.saturating_sub(alive);
                let room = MAX_WORKERS.saturating_sub(alive);
                for _ in 0..missing.min(room) {
                    Scheduler::spawn_worker(&self.inner, self.inner.base);
                }
            }
            handle
        }
    }

    pub fn live_tasks(&self) -> usize {
        *self.inner.live.lock().unwrap()
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
/// running after the deadline (0 when everything finished).
pub fn drain_at_exit() -> usize {
    let ms = drain_ms();
    let sched = Scheduler::shared();
    if ms == 0 {
        return sched.live_tasks();
    }
    sched.drain(std::time::Duration::from_millis(ms))
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
#[cfg(not(target_arch = "wasm32"))]
fn worker_loop(inner: Arc<SchedInner>, base: usize) {
    loop {
        let job = {
            let mut q = inner.queue.lock().unwrap();
            loop {
                if let Some(j) = q.pop_front() {
                    break Some(j);
                }
                let (guard, timeout) = inner.cv.wait_timeout(q, RETIRE_AFTER).unwrap();
                q = guard;
                if timeout.timed_out() && q.is_empty() {
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

/// Sleep for `ms` milliseconds. A Spid3r-level call, not a shell command.
pub fn sleep_ms(ms: i64) {
    if ms > 0 {
        #[cfg(not(target_arch = "wasm32"))]
        thread::sleep(std::time::Duration::from_millis(ms as u64));
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
