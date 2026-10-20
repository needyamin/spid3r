# Stage 2 — concurrency semantics (AUDIT_2026-10-20 §6)

Date: 2026-10-20 · Scope: A-3, A-4, A-7, A-9 + Stage-2 regression tests + full gates.
Every claim below is backed by an executed program or test; the "before" column
was produced by building commit `d292be2` in a separate `git worktree` and running
the same probes against it.

## 1. What was fixed

| ID | Defect | Fix (file) |
|---|---|---|
| A-4 | `gc_collect()` swept while tasks were live (auto-collect was gated, the builtin was not) | `src/stdlib/mod.rs`: refuse with a clean error when `live_tasks() > 0` |
| A-3 | Blocking waits (`recv`/`join`/mutex/sleep) parked pool workers; queued work starved forever | `src/sched/mod.rs`: grow on demand with the invariant `alive >= busy + queued`, capped at `MAX_WORKERS=256`; grown workers retire after 10 s idle |
| A-3 | No join at program exit → tasks killed mid-flight, output lost | `src/sched/mod.rs`: `drain()` on a condvar + `join_tasks_at_exit()`; wired into the tree engine (`run_program`, `run_program_inline`), the VM (`run_compiled`) and the AOT runtime (`spid3r_rt_start`) |
| A-7 | Self-referential container methods re-locked the receiver mutex → permanent hang | `src/value/collections/{mod,seq,misc,nodes}.rs` + `src/value/mod.rs`: snapshot under the lock, compare/clone outside it (list/vec/stack/queue/deque/ring/set/bag/graph/binary_tree); `Value::Graph` deep clone no longer re-locks |
| A-9 | Thrown values were stringified across spawn/await/join/callback seams | `TaskOutcome{result,thrown}` + `thrown_value()` (`src/sched`); structural rethrow in interpreter (`await`, method seam), VM (`Await`, `CallMethod`, callback host), AOT (`spid3r_await`, `spid3r_method`); tree callback host now stashes the thrown value like the VM one |

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
- `gc_collect_refuses_while_tasks_are_live` — clean, catchable error; works after join
- `thrown_values_cross_seams_structurally` — await / join / callback / nested / comparator / string

`tests/native_tests.rs::native_matches_tree_on_structured_task_errors` — the AOT
backend's `await` and `task.join()` seams must match the tree engine.

Validation of the tests themselves: copied into the pre-fix worktree they **all
fail** (`timed out (possible deadlock)` / wrong output); in the fixed tree they
all pass. A test that cannot fail was not accepted as evidence.

## 4. Gates (executed, this working tree)

| Gate | Result |
|---|---|
| `cargo build --no-default-features --all-targets` | OK (only the pre-existing `spid3r.pdb` bin/lib collision warning) |
| `cargo clippy --no-default-features --all-targets -- -D warnings` | 0 warnings |
| `cargo clippy --no-default-features --features native --all-targets -- -D warnings` | 0 warnings |
| `cargo test --no-default-features` | **449 passed, 0 failed** |
| `cargo test --no-default-features --features gui --test gui_tests` | 10/10 |
| `cargo test --no-default-features --features native --test native_tests` | 5/5 |
| `spid3r test` (tree == vm == exec) | 25 ok, 0 failed |
| `spid3r fmt --check` | all unchanged, exit 0 |
| `cargo build --target wasm32-unknown-unknown --release --lib --no-default-features` | OK |
| `cargo fmt --check` | still fails repo-wide — pre-existing F1 (§3 row 8 of the audit), unchanged by this round |

**Testing caveat (observed while validating this round):** two `cargo` commands
must not run against this `target/` concurrently. The bin/lib `spid3r.pdb`
output collision (audit F2) lets a second build overwrite `target/debug/spid3r.exe`
while a test is spawning it, which produced spurious `difftest` /
`native_matches_tree_on_supported_examples` failures that disappear when the
gates run sequentially. All numbers above are from sequential runs.

## 5. Deliberate limits (not faked, documented)

- **Job queue is still unbounded.** Backpressure on `spawn` cannot be added
  safely without continuations: a producer that blocks on a full queue may
  itself be a pool worker, and blocking every worker on queue space deadlocks.
  Memory growth is therefore bounded only by `MAX_WORKERS` threads + the queue;
  a spawn storm still allocates. Recorded as remaining work.
- **Worker threads are not joined at exit.** The pool is a process-wide
  `OnceLock` singleton shared by REPL/embed/test runs, so shutting it down would
  break subsequent runs. Exit drains *tasks* (the thing that owns user work);
  the OS reclaims idle workers when the process ends.
- **> `MAX_WORKERS` concurrently blocked tasks can still stall** — unchanged
  documented limit; the drain warning makes it visible instead of silent.
- **Collection methods are not atomic under concurrent mutation.** The A-7 fix
  snapshots, then commits; `remove` re-checks the slot by identity and fails
  cleanly if it moved. Set `add` can duplicate under a race (documented in code).
- `gc_collect()` inside a task always fails, even when it is the only live task:
  the gate is "no tasks running", not "no *other* tasks".
