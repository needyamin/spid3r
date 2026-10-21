//! Scheduler limit tests: the bounded ready queue, worker shutdown, and the
//! restartable singleton (audit A-3 follow-up — the "deliberate limits").
//!
//! These drive [`Scheduler::isolated`] so the assertions are deterministic: the
//! pool size, the queue bound, and the absence of growth are all explicit
//! instead of depending on the machine's CPU count. Behaviour that must hold
//! for the real CLI (GC gate, stall warning) is covered end-to-end in
//! `tests/stage2_concurrency.rs`.

use spid3r_lang::sched::{Scheduler, TaskOutcome};
use spid3r_lang::value::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn ok_job(counter: Arc<AtomicUsize>) -> Box<dyn FnOnce() -> TaskOutcome + Send + 'static> {
    Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        TaskOutcome {
            result: Ok(Value::Null),
            thrown: None,
        }
    })
}

/// The ready queue must never exceed its bound, and a producer must actually
/// park when it is full (backpressure, not silent growth): with one worker and
/// a cap of one, a second spawn blocks until the worker frees a slot, and every
/// job still runs exactly once.
#[test]
fn queue_backpressure_parks_the_producer_and_loses_no_work() {
    let sched = Scheduler::isolated(1, 1);
    assert_eq!(sched.queue_cap(), 1);
    assert_eq!(sched.max_workers(), 1);

    let done = Arc::new(AtomicUsize::new(0));

    // Occupy the only worker with a job that parks until released.
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let gate_done = done.clone();
    let gate = sched.spawn_job(Box::new(move || {
        let _ = started_tx.send(());
        let _ = release_rx.recv();
        gate_done.fetch_add(1, Ordering::SeqCst);
        TaskOutcome {
            result: Ok(Value::Null),
            thrown: None,
        }
    }));
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker must start the gating job");
    assert_eq!(sched.queued_jobs(), 0);

    // Fill the queue to its cap (the worker is parked, so nothing drains).
    let second = sched.spawn_job(ok_job(done.clone()));
    assert_eq!(sched.queued_jobs(), 1);

    // The next spawn has nowhere to go: it must block in the producer.
    let producer = {
        let sched = sched.clone();
        let done = done.clone();
        std::thread::spawn(move || sched.spawn_job(ok_job(done)))
    };
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !producer.is_finished(),
        "spawn must block while the queue is at its cap"
    );
    assert_eq!(sched.queued_jobs(), 1, "queue must not grow past its cap");

    // Releasing the worker drains the queue and wakes the producer.
    release_tx.send(()).unwrap();
    let third = producer.join().expect("producer thread");
    for h in [gate, second, third] {
        h.join().expect("job must succeed");
    }
    assert_eq!(done.load(Ordering::SeqCst), 3, "every job must run once");
    assert_eq!(sched.queued_jobs(), 0);
    assert!(sched.shutdown());
}

/// A burst of spawns through a small queue on a small pool must still run every
/// job exactly once (the cap slows the producer down, it does not drop work).
#[test]
fn queue_backpressure_survives_a_burst() {
    let sched = Scheduler::isolated(2, 4);
    let done = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..500 {
        handles.push(sched.spawn_job(ok_job(done.clone())));
        assert!(
            sched.queued_jobs() <= sched.queue_cap(),
            "queue exceeded its cap"
        );
    }
    for h in &handles {
        h.join().expect("job must succeed");
    }
    assert_eq!(done.load(Ordering::SeqCst), 500, "every job must run once");
    assert!(sched.shutdown());
}

/// Shutdown must stop and join every worker thread: no thread may survive it.
#[test]
fn shutdown_joins_all_workers() {
    let sched = Scheduler::isolated(4, 64);
    assert_eq!(sched.worker_count(), 4);

    let done = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..16 {
        handles.push(sched.spawn_job(ok_job(done.clone())));
    }
    for h in &handles {
        h.join().expect("job must succeed");
    }
    assert_eq!(done.load(Ordering::SeqCst), 16);
    assert_eq!(sched.live_tasks(), 0);

    assert!(sched.shutdown(), "a drained pool must shut down cleanly");
    assert!(sched.is_stopped());
    assert_eq!(sched.worker_count(), 0, "all workers must have exited");

    // Idempotent: a second shutdown is a no-op, not a hang or a panic.
    assert!(sched.shutdown());
}

/// `shutdown` must not block forever on a pool whose task is stuck: it reports
/// the failure to join and leaves that thread to process exit.
#[test]
fn shutdown_reports_a_stuck_task_instead_of_hanging() {
    let sched = Scheduler::isolated(1, 8);
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    // A job that parks until the test releases it.
    let handle = sched.spawn_job(Box::new(move || {
        let _ = started_tx.send(());
        let _ = rx.recv();
        TaskOutcome {
            result: Ok(Value::Null),
            thrown: None,
        }
    }));
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker must enter the stuck job");

    let started = Instant::now();
    let joined = sched.shutdown();
    assert!(
        !joined,
        "shutdown must report that a running task could not be joined"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "shutdown must not block on a stuck task (took {:?})",
        started.elapsed()
    );

    // Release the task so the thread can finish; the stopped pool exits after
    // the job instead of taking more work.
    drop(tx);
    let _ = handle;
}

/// The process-wide singleton must come back after a clean shutdown, so a REPL
/// or embedding host can keep spawning tasks.
#[test]
fn shared_scheduler_restarts_after_shutdown() {
    let first = Scheduler::shared();
    let done = Arc::new(AtomicUsize::new(0));
    first
        .spawn_job(ok_job(done.clone()))
        .join()
        .expect("job must succeed");
    assert_eq!(done.load(Ordering::SeqCst), 1);

    // `join()` returns as soon as the result is published; the pool's task
    // record may take a moment longer to settle, during which `shutdown()`
    // legitimately reports "a task could not be joined". Retry until it can
    // (shutdown is documented as safe to repeat) — on a loaded CI runner the
    // gap is wide enough to observe.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut shut = first.shutdown();
    while !shut && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        shut = first.shutdown();
    }
    assert!(
        shut,
        "shutdown must join every worker once the task finished"
    );
    assert!(first.is_stopped());

    let second = Scheduler::shared();
    assert!(
        !Arc::ptr_eq(&first, &second),
        "shared() must hand out a fresh pool after shutdown"
    );
    let done2 = Arc::new(AtomicUsize::new(0));
    second
        .spawn_job(ok_job(done2.clone()))
        .join()
        .expect("job must succeed on the restarted pool");
    assert_eq!(done2.load(Ordering::SeqCst), 1);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut shut = second.shutdown();
    while !shut && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        shut = second.shutdown();
    }
    assert!(shut, "the restarted pool must shut down cleanly");
}

/// A stopped pool must not queue work no thread will ever run: `spawn_job`
/// degrades to running the job on the caller's thread, so `join` still returns.
#[test]
fn spawning_on_a_stopped_pool_runs_the_job_instead_of_hanging() {
    let sched = Scheduler::isolated(2, 16);
    assert!(sched.shutdown());

    let done = Arc::new(AtomicUsize::new(0));
    let handle = sched.spawn_job(ok_job(done.clone()));
    handle
        .join()
        .expect("a job spawned after shutdown must still complete");
    assert_eq!(done.load(Ordering::SeqCst), 1);
    assert_eq!(sched.live_tasks(), 0);
    assert_eq!(sched.queued_jobs(), 0);
}

/// A *second* shutdown after a failed one must not try to join the worker that
/// is still stuck (that would hang the exit path it is supposed to protect).
#[test]
fn a_repeated_shutdown_still_does_not_join_a_stuck_task() {
    let sched = Scheduler::isolated(1, 8);
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let _handle = sched.spawn_job(Box::new(move || {
        let _ = started_tx.send(());
        let _ = rx.recv();
        TaskOutcome {
            result: Ok(Value::Null),
            thrown: None,
        }
    }));
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker must enter the stuck job");

    assert!(!sched.shutdown(), "first shutdown cannot join a stuck task");
    let started = Instant::now();
    assert!(
        !sched.shutdown(),
        "the repeat must report the same failure, not pretend success"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the repeat shutdown blocked on the stuck worker (took {:?})",
        started.elapsed()
    );

    drop(tx);
}
