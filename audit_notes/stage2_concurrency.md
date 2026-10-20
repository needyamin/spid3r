# Stage 2 — concurrency semantics (AUDIT_2026-10-20 §6)

Date: 2026-10-20 · Scope: A-3, A-4, A-7, A-9 + Stage-2 regression tests + full gates.
  
Every claim below is backed by an executed program or test; the "before" column
  
was produced by building commit `d292be2` in a separate `git worktree` and running
  
the same probes against it.

## 1. What was fixed

| ID  | Defect                                                                                      | Fix (file)                                                                                                                                                                                                                                                                 |
| --- | ------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A-4 | `gc_collect()` swept while tasks were live (auto-collect was gated, the builtin was not)    | `src/stdlib/mod.rs`: refuse with a clean error when `live_tasks() > 0`                                                                                                                                                                                                     |
| A-3 | Blocking waits (`recv`/`join`/mutex/sleep) parked pool workers; queued work starved forever | `src/sched/mod.rs`: grow on demand with the invariant `alive >= busy + queued`, capped at `MAX_WORKERS=256`; grown workers retire after 10 s idle                                                                                                                          |
| A-3 | No join at program exit → tasks killed mid-flight, output lost                              | `src/sched/mod.rs`: `drain()` on a condvar + `join_tasks_at_exit()`; wired into the tree engine (`run_program`, `run_program_inline`), the VM (`run_compiled`) and the AOT runtime (`spid3r_rt_start`)                                                                     |
| A-7 | Self-referential container methods re-locked the receiver mutex → permanent hang            | `src/value/collections/{mod,seq,misc,nodes}.rs` + `src/value/mod.rs`: snapshot under the lock, compare/clone outside it (list/vec/stack/queue/deque/ring/set/bag/graph/binary_tree); `Value::Graph` deep clone no longer re-locks                                          |
| A-9 | Thrown values were stringified across spawn/await/join/callback seams                       | `TaskOutcome{result,thrown}` + `thrown_value()` (`src/sched`); structural rethrow in interpreter (`await`, method seam), VM (`Await`, `CallMethod`, callback host), AOT (`spid3r_await`, `spid3r_method`); tree callback host now stashes the thrown value like the VM one |

Supporting hygiene:

- `same_slot()` (`src/value/mod.rs`): identity/scalar check used to commit an
    
  index resolved on a snapshot, so a concurrent mutation yields a clean
    
  "changed concurrently; retry" error instead of removing the wrong element.
- `clear_error_thrown()` at method-dispatch entry: a stashed thrown value can
    
  never attach itself to an unrelated later error on the same thread.
- `bag.remove` guards against `usize` underflow (host panic from script input).
- `task_finished()` uses `saturating_sub` so the live-task counter cannot wrap.

## 2. Before/after evidence (real programs, both engines)

Baseline = `git worktree` at `d292be2`; "after" = working tree.

```
case                              baseline            after
list_slice_self                   HANG (tree+vm)      1
stack_to_list_self                HANG (tree+vm)      1
queue_to_list_self                HANG (tree+vm)      1
deque_to_list_self                HANG (tree+vm)      1
set_to_list_self                  HANG (tree+vm)      1
bintree_inorder_self              HANG (tree+vm)      1
graph_nodes_self                  HANG (tree+vm)      1
graph_clone_self                  HANG (tree+vm)      1
grow-on-demand (26 blocked tasks) HANG (tree+vm)      42
exit drain (task sleeps 200 ms)   "main-output" only  "main-output" + "task-output"
gc_collect with live task         "no-error"          "blocked" (clean error)
throw {code:7} across await       catch gets string   catch gets map, e[code]==7
throw {code:9} across task.join   catch gets string   catch gets map, e[code]==9
throw {code:11} across callback   catch gets string   catch gets map
throw {code:13} nested callbacks  catch gets string   catch gets map
throw {code:17} comparator        catch gets string   catch gets map
throw "plain" across await        "task failed: plain" "plain"
```

Probes: `audit_notes/probes/stage2_probe.py` (21 hang families) and
  
`audit_notes/probes/stage2_semantics.py` (8 semantic cases), each run per engine
  
with a watchdog. Final sweep: **0 hangs**.

## 3. Regression tests added

`tests/stage2_concurrency.rs` — 6 tests, every one runs a real `spid3r run` child
  
process under a watchdog and kills it on timeout, so a reintroduced hang fails
  
the suite instead of blocking CI:

- `self_referential_container_methods_terminate` — 16 families × both engines
- `grow_on_demand_runs_work_queued_behind_blocked_tasks` — pool size + 6 blocked tasks
- `program_exit_waits_for_running_tasks` — task output must survive `main` returning
- `exit_reports_tasks_that_never_finish` — bounded drain, warning, exit 0
- `gc_collect_runs_while_tasks_are_parked_and_refuses_while_they_run` — precise gate
- `a_task_can_collect_when_no_other_thread_is_running` — collection from inside a task
- `thrown_values_cross_seams_structurally` — await / join / callback / nested / comparator / string
- `bounded_queue_paces_a_spawn_storm_without_losing_work` — cap 4, 400 tasks, both engines
- `concurrent_set_adds_do_not_duplicate` / `concurrent_bag_adds_keep_an_exact_count`
- `a_stalled_pool_is_reported_instead_of_hanging_silently` — watchdog message at the ceiling

`tests/sched_limits.rs` (7, isolated pools): the queue cap parks the producer
  
(and does not lose work), 500-job burst under a cap of 4, shutdown joins every
  
worker, shutdown is idempotent, shutdown does not block on a stuck task, and the
  
singleton restarts after a stop.

`src/value/collections/seq.rs` unit tests (4): a *stale snapshot* add/remove must
  
report `CONFLICT` rather than duplicate or remove a shifted element — the
  
deterministic version of the concurrent cases.

`tests/native_tests.rs::native_matches_tree_on_structured_task_errors` — the AOT
  
backend's `await` and `task.join()` seams must match the tree engine.

Validation of the tests themselves: copied into the pre-fix worktree they **all
  
fail** (`timed out (possible deadlock)` / wrong output); in the fixed tree they
  
all pass. A test that cannot fail was not accepted as evidence.

## 4. Gates (executed, this working tree, 2026-10-21)

| Gate                                                                                | Result                                                                                   |
| ----------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| `cargo build --no-default-features --all-targets`                                   | OK (only the pre-existing `spid3r.pdb` bin/lib collision warning)                        |
| `cargo clippy --no-default-features --all-targets -- -D warnings`                   | 0 warnings                                                                               |
| `cargo clippy --no-default-features --features native --all-targets -- -D warnings` | 0 warnings                                                                               |
| `cargo test --no-default-features`                                                  | **465 passed, 0 failed**                                                                 |
| `cargo test --no-default-features --features gui --test gui_tests`                  | 10/10                                                                                    |
| `cargo test --no-default-features --features native --test native_tests`            | 5/5                                                                                      |
| `spid3r test` (tree == vm == exec)                                                  | 25 ok, 0 failed                                                                          |
| `spid3r fmt --check`                                                                | all unchanged, exit 0                                                                    |
| `cargo build --target wasm32-unknown-unknown --release --lib --no-default-features` | OK                                                                                       |
| `cargo fmt --check`                                                                 | failed repo-wide at the time (pre-existing F1) — fixed in the Stage-4 rustfmt normalization, and now gated in CI |

**Testing caveat (observed while validating this round):** two `cargo` commands
  
must not run against this `target/` concurrently. The bin/lib `spid3r.pdb`
  
output collision (audit F2) lets a second build overwrite `target/debug/spid3r.exe`
  
while a test is spawning it, which produced spurious `difftest` /
  
`native_matches_tree_on_supported_examples` failures that disappear when the
  
gates run sequentially. All numbers above are from sequential runs.

## 5. Limits from round 1 — resolved 2026-10-21

Round 1 shipped four honest limits. All four were then fixed for real; each
  
entry states the mechanism, the evidence, and what (if anything) still remains.


| Round-1 limit                                                             | Fix                                                                                                                                                                                                                                                                                                                                           | Evidence                                                                                                                                                                                                                                                                                                                                             |
| ------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Ready queue unbounded: a spawn storm grew memory without limit            | Bounded queue (`SPID3R_QUEUE_CAP`, default 65536) with producer backpressure. `spawn` grows the pool first, then parks on `space_cv` until a worker takes a job (workers notify while holding the queue lock, so no lost wakeup).                                                                                                             | `sched_limits::queue_backpressure_parks_the_producer_and_loses_no_work` (queue=1, worker gated: producer provably still parked, queue never > 1, all 3 jobs run) + `queue_backpressure_survives_a_burst` (500 jobs, cap 4) + `stage2_concurrency::bounded_queue_paces_a_spawn_storm_without_losing_work` (both engines, cap 4, 400 tasks)            |
| Worker threads not joined at exit (killed mid-write)                      | `Scheduler::shutdown()` sets a stopping flag, wakes workers/producers/watchdog, and joins every worker handle. Called by the CLI after the last command and by the AOT runtime after `main`. The singleton is restartable (`shared()` rebuilds after a stop) so REPL/embed keep working.                                                      | `sched_limits::shutdown_joins_all_workers` (16 jobs, 4 workers → `worker_count() == 0`), `shutdown_reports_a_stuck_task_instead_of_hanging`, `shared_scheduler_restarts_after_shutdown`                                                                                                                                                              |
| `> MAX_WORKERS` blocked tasks could stall silently                        | The ceiling is configurable (`SPID3R_MAX_WORKERS`) and a watchdog samples once per second: queued work + every worker busy + at the ceiling for ~3 s prints an actionable stderr warning (`... raise SPID3R_MAX_WORKERS ...`) instead of hanging with no diagnostic.                                                                          | `stage2_concurrency::a_stalled_pool_is_reported_instead_of_hanging_silently` (MAX=2, 3 blocked tasks → warning on stderr, program still completes). Pre-fix binary: no output at all.                                                                                                                                                                |
| `set.add` could duplicate under a race (and `bag.add` could double-count) | Detached mutators now commit through a verified snapshot (compare-and-swap): `set.add` re-checks the whole snapshot before inserting; `bag.add` re-checks the target entry; `remove` re-checks the slot by identity/scalar. Conflicts retry with a fresh snapshot (bounded at 32) instead of corrupting.                                      | Deterministic: `src/value/collections/seq.rs` unit tests drive a *stale* snapshot directly (pre-fix code pushes a duplicate — verified in a worktree; fixed code returns `CONFLICT`). Probabilistic: `stage2_concurrency::concurrent_set_adds_do_not_duplicate` (8×200 adds → 1) and `concurrent_bag_adds_keep_an_exact_count` (4×50 → exactly 200). |
| `gc_collect()` inside a task always failed                                | The gate is now the actual mark-sweep precondition: no *other* thread is executing Spid3r code. `enter_active`/`enter_blocked` track running vs parked threads; `recv`, `join`, `mutex.lock` and `sleep_ms` mark themselves parked. A task can collect when everyone else is parked, and the main thread can collect while tasks are blocked. | `stage2_concurrency::gc_collect_runs_while_tasks_are_parked_and_refuses_while_they_run` (parked → `true`, running → refused, idle → `true`) and `a_task_can_collect_when_no_other_thread_is_running`. Pre-fix: both refused.                                                                                                                         |

### What genuinely remains

- **Removing the worker ceiling entirely needs M:N scheduling.** A task that
    
  blocks on `recv`/`join`/`mutex` occupies an OS thread, because the tree
    
  interpreter and the VM cannot suspend mid-frame without a continuation
    
  mechanism (stackful coroutines or a rewritten state machine). Until then the
    
  ceiling is a real capacity limit; it is now configurable and its stalls are
    
  reported with a fix, instead of being silent.
- **Hosts that drive the interpreter below `run_program` are not counted.** The
    
  GC gate counts threads that entered through `run_program`,
    
  `run_program_inline`, `Vm::run_compiled`, an AOT `main`, or a spawned job. A
    
  host calling `Interp::call_function` directly is treated as a single-threaded
    
  caller; if it runs several such threads, it must call `sched::enter_active`
    
  itself.
- **Blocking I/O is conservatively *active*.** A thread inside `http_fetch` or
    
  `exec_capture` is not marked parked (it can call back into Spid3r code via
    
  callbacks), so it keeps `gc_collect()` from running on another thread. This
    
  errs on the safe side: it can only refuse a collection, never allow an unsafe
    
  one.
- **Collection methods are linearizable per operation, not per sequence.** Two
    
  separate calls from the same task can interleave with another task's calls;
    
  there is no transaction/atomic-block construct.
