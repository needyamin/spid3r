# Stage 0 — correctness debt (A-13…A-17 + low-severity batch)

Date: 2026-10-21 · Toolchain: rustc/cargo 1.99.1 (pinned) · Base: `f17f8cf`
Scope: close the five findings the 2026-10-20 audit left open, plus the
low-severity items and the doc drift it recorded. No new language features.

Every "before" number below was measured on a separate `git worktree` at the
commit named in the row, not inferred from the audit text.

## 1. A-17 — deep container chains aborted the process

`stack`/`queue`/`deque`/`bag`/instance chains died in `Drop` with
`0xC00000FD` ("has overflowed its stack") **after** printing their result, so the
program looked correct until the process disappeared.

Root cause: three near-duplicate guards existed (`VecData`, `MapData`, `SetData`),
each understanding only its own type plus `list`. A `stack` chain matched none of
them, and a *mixed* chain matched nothing at all.

Fix: one worklist, `collections::drop_iteratively`, that knows every container
variant (list/vec, stack, queue, deque, set, map, bag, ring, heap, prio-queue,
tuple, option/result) and all three instance types; every one of those types now
implements `Drop` in terms of it.

| Probe (200k deep, child process) | before (`847ed67^`) | after |
|---|---|---|
| `stack` / `queue` / `deque` | `0xC00000FD` | exit 0 |
| `bag` | `0xC00000FD` | exit 0 |
| `class Node { next }` chain | `0xC00000FD` | exit 0 |
| mixed list→stack→map→deque→set→bag | `0xC00000FD` | exit 0 |
| 1M-deep `stack` | — | exit 0 |

`tests/deep_drop.rs` (5) runs each probe in a child process, because a
regression aborts the process and would take the whole test binary with it. Three
of the five fail on the pre-fix tree (the map chain and the shared-child case
already passed).

`.npy`: the shape comes from the file header, so `n * 8` and `rows * cols` were
attacker-controlled. In a release build they wrapped — letting an enormous shape
pass the length check — and in a debug build they panicked. Both are
`checked_mul` now, and `save_npy` rejects a shape that disagrees with its payload
instead of writing a file that `load_npy` would reject.

## 2. A-14 — `deep_clone` aliased the original into the copy

`a.push(a); b = a.clone(); b[0].push("x")` gave `a.len()==2`, `b.len()==1`: the
clone's inner element was the **original**, so mutating through the clone mutated
the source. The old implementation used a `HashSet` of visited ids purely to stop
infinite recursion and returned `v.clone()` on a revisit.

Fix: a memo of `orig → clone`, with each container registered **before** its
children are cloned, so a back-reference resolves to the half-built clone
(`copy.deepcopy` semantics). Result: `a.len()==1`, `b.len()==2`, `b[0]==b`.

Consequence worth stating: a value reachable by two paths is now *shared* in the
copy rather than duplicated. Two existing tests had the aliasing encoded in their
expectations (`[[[...]]]` for a cloned cycle, `h.has_node(g)` for a cloned
self-graph); both now assert the cyclic-copy behaviour.

## 3. A-15 — regex was O(n²)

`regex_match/find/find_all/replace` each looped over every start index and re-ran
the NFA from there. `regex_find("a+b", "a"*n)`:

| n | before (`f63ec08^`) | after |
|---|---|---|
| 2 500 | 634 ms | 0 ms |
| 5 000 | 2 553 ms | 1 ms |
| 20 000 | > 30 s (timeout) | 4 ms |
| 100 000 | — | 21 ms |

The engine now carries a "start anywhere" thread through one scan: existing
threads claim states before a new start is injected (so the earliest start wins),
and a `Match` records the thread's start index so the longest end for that start
is reported.

**Semantics were verified, not assumed.** 28 patterns × 19 texts × 4 entry points
(532 cases) produce byte-identical output on the old and new binaries. That
caught two regressions I had introduced:

1. `$` stopped matching non-empty input — the search returned early when no
   thread was live at position 0, before the end-of-input start was ever tried.
2. `replace()` panicked on the empty end-of-input match (`chars[s]` out of
   bounds) and dropped the unmatched prefix.

Both are fixed and pinned by `tests/regex_scaling.rs` (4) and an
`anchors_keep_their_meaning` case. The failing-scan test takes **43 s** on the
pre-fix tree; the whole suite now runs in 0.15 s.

## 4. A-16 — HTTP framing had no deadline

`read_head` blocked until the peer sent something. A client that connects and
stays silent (or dribbles one byte a minute — slowloris) held a server worker
indefinitely.

Fix: `http_read_request` applies `SPID3R_NET_TIMEOUT_MS` (default 30 000, `0`
disables) **only if the program did not already call `tcp_set_timeout`** — an
explicit choice always wins, so a deliberately patient connection is not silently
shortened. The timeout error names both controls.

| Probe (`SPID3R_NET_TIMEOUT_MS=400`) | before (`4d00a05^`) | after |
|---|---|---|
| client connects, sends nothing | 6.18 s (waited for close) | 0.41 s |
| client sends a partial header, then stalls | 6.18 s | 0.40 s |
| complete request | ok | ok (and its own 1500 ms timeout is kept) |

`ChunkReader` also compacted nothing: `pos` advanced but `buf` kept every byte it
had ever read, so an 8 MB chunked body retained ~8 MB it could never use again
and every `take` copied out of a steadily growing vector. It now compacts once
the consumed prefix dominates. `read_head` also never reads past
`MAX_HEAD_BYTES`, so a peer streaming headers cannot overshoot by a chunk per
iteration.

## 5. A-13 — closure cycles: measured, documented, still open

Not fixed. This is the one item I did not complete, and the reason is in
`docs/GC.md`:

```
fn make() { let data = list(); let f = fn() { return data; }; return f; }
```
`env -> closure -> env` is a cycle through two *untracked* allocations. Measured:
20 000 calls leave `gc_live_cells()` at **20 000** — one leaked cell per closure,
not a slow drift. Every nested named function has the same shape.

Registering only environments cannot fix it: the single strong reference to an env
comes from an untracked `FunctionData`, so `external = strong − internal` is 1 and
the env looks like a root. Both ends must be cells, which means registering
`EnvironmentData`, `FunctionData`, `ClassData`/`VmClass` and `VmFunc` and tracing
`env`/`methods`/`vars`/`parent` — and keeping registration *complete*, because a
container holding an unregistered closure cannot be traversed and an env
reachable only through it would be swept while still live (a use-after-free, not
a leak). That is why the note also calls for private fields or `new_closure`
constructors: a future construction site must not be able to skip registration
silently.

I chose the documented-bound path the plan allowed rather than a half-working
collector. `tests/closure_cycle_leak.rs` (3) pins the current number so a real
fix shows up as a test change, and includes the acyclic case to catch
over-collection.

## 6. Low-severity batch

| Item | Fix | Pinned by |
|---|---|---|
| `Parser::parse(vec![])` panicked (`toks.len() - 1`) | empty stream is a diagnostic; `peek_kind` saturates | `low_severity_fixes.rs` |
| Byte-based diagnostic columns | `col` and span `len` count characters; `ααα` line reports `2:20`, not `2:23` | same |
| CSV kept the UTF-8 BOM | `parse_csv` strips a leading U+FEFF (`headers[0]` is `name`, len 4) | same |
| `[^a]` never matched `\n` | negated classes match newline (PCRE/Python/Go); only `.` excludes it | same |
| `read_tar` extracted symlink entries as regular files | regular files accepted, directories/pax skipped, symlink/device/fifo rejected | `pkg_tests` |
| CLI panicked on non-UTF-8 args | `args_os()` + lossy replacement | build |
| `spid3r test` discarded its arguments | `--filter SUBSTR`, `--list`; unknown argument is a usage error | `low_severity_fixes.rs` |

Doc drift the audit found, corrected: `docs/REGISTRY.md` claimed there is no
transitive resolution (the code walks the graph with cycle detection);
`docs/ROADMAP.md` claimed higher-order methods are unsupported natively (they are,
and `xs.map(f)` was verified through `spid3r build`).

## 7. Gates

| Gate | Result |
|---|---|
| `cargo build --no-default-features --all-targets` | clean |
| `cargo clippy --no-default-features --all-targets -- -D warnings` | 0 errors |
| `cargo fmt --all -- --check` | clean |
| `cargo test --no-default-features` | **504 passed, 0 failed** (43 suites) |
| `spid3r test` (three-engine differential) | **25 ok, 0 failed** |
| GUI / AOT | 10/10, 6/6 (unchanged) |

Canonical counts (`tools/count_tests.sh`): **520 tests** (504 core + 10 GUI +
6 AOT), 25 example files, 3 fuzz targets.

## 8. Behaviour changes to be aware of

1. `deep_clone` shares multiply-referenced values instead of duplicating them.
2. `[^...]` now matches `\n`.
3. HTTP framing on a connection with no explicit timeout is bounded at 30 s
   (`SPID3R_NET_TIMEOUT_MS=0` restores the old unbounded behaviour).
4. Diagnostic columns are character-based, so reported columns shift left on
   lines containing non-ASCII text.
5. `spid3r test <arg>` now fails on an unrecognised argument instead of ignoring
   it.
6. `read_tar` rejects symlink/device entries (previously extracted as files with
   the link target as content).

Commits: `847ed67` (A-17 + A-14), `f63ec08` (A-15), `4d00a05` (A-16), plus the
low-severity batch and docs.
