//! The task scheduler. Tasks run on a pool of OS threads (default size:
//! available parallelism) over one shared ready queue. There is no global
//! interpreter lock: two CPU-bound tasks run on two OS threads at once.
//!
//! Documented limits:
//! - `recv` on a channel with no future sender blocks the calling worker
//!   forever (it parks on a condvar, so it does not spin).
//! - channels are unbounded in v1; growth is limited by process memory.
//! - `await` on a task joins it, consuming the joining worker.
//! - a task that fails while holding a mutex releases it when its scope ends
//!   (locks are scope-based, like `finally`).

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
#[cfg(not(target_arch = "wasm32"))]
use std::thread;

use crate::value::Value;

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
    result: Mutex<Option<Result<Value, String>>>,
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
            Some(Ok(_)) => "done",
            Some(Err(_)) => "failed",
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
        r.as_ref().unwrap().clone()
    }
}

type Job = Box<dyn FnOnce() -> Result<Value, String> + Send + 'static>;

#[allow(dead_code)]
struct SchedInner {
    queue: Mutex<VecDeque<Job>>,
    cv: Condvar,
    next_id: Mutex<usize>,
    live: Mutex<usize>,
}

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
        let inner = Arc::new(SchedInner {
            queue: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            next_id: Mutex::new(1),
            live: Mutex::new(0),
        });
        #[cfg(not(target_arch = "wasm32"))]
        {
            let pool = thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1);
            for _ in 0..pool {
                let inner = inner.clone();
                thread::Builder::new()
                    .name("spid3r-worker".to_string())
                    .stack_size(WORKER_STACK_BYTES)
                    .spawn(move || loop {
                        let job = {
                            let mut q = inner.queue.lock().unwrap();
                            while q.is_empty() {
                                q = inner.cv.wait(q).unwrap();
                            }
                            q.pop_front()
                        };
                        match job {
                            Some(job) => {
                                let _ = job();
                            }
                            None => continue,
                        }
                    })
                    .expect("failed to start a spid3r worker thread");
            }
        }
        Arc::new(Scheduler { inner })
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
            let stored: Result<Value, String> = match result {
                Ok(r) => r,
                Err(_) => Err("task panicked".to_string()),
            };
            {
                let mut r = state.result.lock().unwrap();
                *r = Some(stored);
            }
            *self.inner.live.lock().unwrap() -= 1;
            handle
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let inner = self.inner.clone();
            let wrapped: Job = Box::new(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                let stored: Result<Value, String> = match result {
                    Ok(r) => r,
                    Err(_) => Err("task panicked".to_string()),
                };
                {
                    let mut r = state.result.lock().unwrap();
                    *r = Some(stored);
                    state.cv.notify_all();
                }
                *inner.live.lock().unwrap() -= 1;
                Ok(Value::Null)
            });
            self.inner.queue.lock().unwrap().push_back(wrapped);
            self.inner.cv.notify_one();
            handle
        }
    }

    pub fn live_tasks(&self) -> usize {
        *self.inner.live.lock().unwrap()
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
