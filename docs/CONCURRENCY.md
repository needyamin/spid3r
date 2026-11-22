# Spid3r Concurrency & Scheduler Model

> **Status:** Production runtime specification verified against [`src/sched/mod.rs`](../src/sched/mod.rs), [`src/stdlib/eventloop.rs`](../src/stdlib/eventloop.rs), [`src/stdlib/net.rs`](../src/stdlib/net.rs), and [`stdlib/http.matrix`](../stdlib/http.matrix).

---

## 1. Execution Model Overview

Spid3r provides **true multi-core OS-thread task parallelism with no Global Interpreter Lock (GIL)** on native targets (`tree`, `vm`, and LLVM `native` AOT), falling back cleanly to inline synchronous execution on single-threaded `wasm32`:

- Two CPU-bound spawned tasks execute simultaneously across distinct OS threads (`spid3r-worker`, `WORKER_STACK_BYTES = 32 MiB`).
- Spawning the first task calls `value::mark_multithreaded()`, permanently transitioning container locking (`FastMutex`) from its lock-free single-threaded fast path into full OS/atomic synchronization before the task executes.

---

## 2. Work-Pool Architecture & Backpressure (`src/sched/mod.rs`)

### 2.1 Pool Sizing & Dynamic Growth
- **Base Pool Size:** `thread::available_parallelism()` (clamped to `[1, max_workers]`).
- **Dynamic Growth on Blocking Waits:** To prevent thread-starvation deadlocks when tasks park in `recv()`, `mutex.lock()`, or `.join()`, enqueueing checks the invariant `alive >= busy + queued`. If queued tasks exceed available idle workers, new workers spawn up to `SPID3R_MAX_WORKERS` (default `DEFAULT_MAX_WORKERS = 256`, configurable via env or `spid3r.max_workers` in `spid3r.ini`).
- **Worker Retirement:** Idle workers above the base parallelism count automatically retire after `RETIRE_AFTER = 10 s`, reclaiming stack/thread resources after burst concurrency subsides.

### 2.2 Bounded Queue & Backpressure
- The shared ready queue is strictly bounded by `SPID3R_QUEUE_CAP` (default `DEFAULT_QUEUE_CAP = 65536`, configurable via env or `spid3r.queue_cap`).
- When the queue reaches capacity, `Scheduler::enqueue` grows the worker pool toward `max_workers` first and then parks producers on `space_cv` until workers pop queued jobs—preventing spawn storms from exhausting process memory.

---

## 3. Synchronization & Communication Primitives

| Primitive | Creation | Semantics & Safety Invariants |
| :--- | :--- | :--- |
| **Tasks (`spawn` / `.join()` / `await`)** | `spawn fn() { ... };` | Runs closure on OS thread pool; returns `TaskHandle` (`status()`, `is_done()`, `cancel()`, `is_cancelled()`, `join()`). `join()` is repeatable (clones stored `TaskOutcome`) and rethrows the original `Value` payload if the task threw (`TaskOutcome::thrown`). |
| **Channels (`channel()`)** | `channel()` | Unbounded thread-safe FIFO (`Mutex<VecDeque<Value>>` + `Condvar`). `.recv()` parks the worker cleanly (`wait_timeout` in 50 ms cancellation-check slices) without busy-spinning and is aborted cleanly when cancelled; `.try_recv()`, `.send(v)`, `.len()`, `.is_empty()`. |
| **Mutexes (`mutex(v)`)** | `mutex(initial)` | Wraps a value in mutual exclusion. **Non-reentrant:** re-locking a mutex already owned by `current_owner_id()` immediately returns the catchable error `"mutex: deadlock - this task already holds the lock (mutexes are not reentrant)"` rather than self-deadlocking. Held locks track via `MutexGuardToken` and release deterministically when the scope ends, throws, or unwinds. |
| **Atomics (`atomic_int`)** | `atomic_int(init)` | Lock-free `Arc<AtomicI64>` counter for low-overhead atomic integer updates across tasks. |

### 3.1 Shared Mutable State Rule
Lexical closure captures are cell-shared across threads (`AtomicPtr<Value>` / `UpvalueCell`), guaranteeing memory safety (no torn pointers or use-after-free). However, compound read-modify-write updates (`total = total + 1`) are **not atomic** at language level and require `mutex()` or `atomic_int()` for race-free counters ([SECURITY.md](SECURITY.md)).

---

## 4. Deadlock Watchdog & Clean Exit Drain

### 4.1 Untimed vs Timed Parks & Deadlock Detection
The scheduler distinguishes two classes of waiting:
- **Untimed Parks (`enter_untimed_park`):** Waits that **only another Spid3r task can satisfy** (`channel.recv`, `mutex.lock`, `task.join`).
- **Timed / External Parks:** Waits woken by OS timers or external I/O (`sleep_ms`, `tcp_accept*`, `tcp_read`, `tcp_poll`, `event_loop_run`). These **never** increment `UNTIMED_PARKED`, so idle servers never trigger false-positive deadlock aborts.

`watchdog_loop` samples every `WATCHDOG_TICK = 1 s`:
1. **Worker Ceiling Stall Warning:** After `WATCHDOG_STALL_TICKS = 3` consecutive seconds with `queued > 0`, `busy >= alive`, and `alive >= max_workers`, logs an actionable stderr diagnostic naming the queue depth and `SPID3R_MAX_WORKERS`.
2. **Fatal Deadlock Detection:** After `3` consecutive ticks with `queued == 0`, `busy == 0`, `live == 0`, and `untimed > 0` (meaning non-worker threads such as `main` are blocked forever with zero tasks able to wake them), exits with status `1` and diagnostic `"spid3r: fatal: deadlock - ..."`. Setting `SPID3R_DEADLOCK_ABORT=0` (or `spid3r.deadlock_abort = false`) downgrades this abort to a one-time warning for custom embedding hosts.

### 4.2 Program Exit Task Drain
When `main` returns while background tasks are still running, `join_tasks_at_exit()` waits on `sched.idle` condvar up to `DEFAULT_DRAIN_MS = 2000 ms` (overridable via `SPID3R_TASK_DRAIN_MS` or `spid3r.task_drain_ms`). Remaining unfinished tasks after the deadline emit a warning, and `Scheduler::shutdown()` cleanly joins finished worker threads without deadlocking teardown on permanently blocked threads.

---

## 5. GC Quiescence Protocol (`enter_active` / `enter_blocked`)

Because Spid3r's cycle collector reads `Arc` strong counts without halting OS threads via platform signals:
- Every active engine/task frame holds an `ActiveGuard` (`enter_active()`, incrementing `ACTIVE`).
- Every blocking wait (`recv`, `join`, `mutex.lock`, `sleep_ms`, socket poll/read/accept) drops `ACTIVE` via `BlockedGuard` (`enter_blocked()`), leaving any held `Value`s protected on that thread's stack as external references.
- Cycle collection proceeds strictly when `gc_quiescent()` holds (`ACTIVE == 1`, meaning no other thread is concurrently mutating/cloning/dropping tracked cells).

---

## 6. Event Loop & Concurrent HTTP Server Architecture

- **Kernel Event Loop (`src/stdlib/eventloop.rs`):** A dedicated OS thread monitors readiness sets (`epoll` on Linux, `kqueue` on macOS, `WSAPoll` on Windows; 2 ms probe fallback on `wasm32`) and dispatches ready socket/timer callbacks onto `Scheduler` with level-triggered disarm-while-dispatching semantics, parking callers under `enter_blocked()`.
- **TCP / HTTP Readiness & Buffer Recycling (`src/stdlib/net.rs`):**
  - `tcp_accept_timeout` and `tcp_poll` use `WSAPoll` / `poll(2)` under `enter_blocked()` to yield CPU and GC quiescence cleanly while waiting for client traffic.
  - Per-connection 4 KiB read/write `BufGuard` buffers recycle across HTTP/1.1 keep-alive requests (capped at a 64 KiB shrink watermark), combined with SWAR `\r\n\r\n` header framing and cached `ThinArcStr` HTTP method/version tokens to keep heap churn bounded under sustained keep-alive load.
- **Multi-Worker `HttpServer` (`stdlib/http.matrix`):**
  - `HttpServer.serve(-1)` spawns a configurable accept/worker pool via `opts["workers"]` (default **`4`** for unbounded non-TLS listeners, clamped to `[1, 16]`), with each accepted connection handled cleanly on the scheduler task pool alongside `O(1)` static route table dispatch (`static_routes` / `static_paths`), bounded idle/request timeouts, and graceful `drain_ms` shutdown.
