# 06 — Ecosystem: Package Manager, Module Loader, Scheduler, REPL, CLI

> **Status 2026-10-21 (verification round).** This file is the 2026-10-20
> static record and is not rewritten. What changed since:
>
> - **Fixed:** **F1** (`ext::` git URL → command execution — audit A-5; rejected
>   unconditionally, with a test that proves no command ran), **F2** (scheduler starvation
>   / unbounded queue / no exit join — audit A-3), **F3** (lockfile hashed lossy UTF-8 of
>   only `*.matrix` files — now every file under `src/` as raw bytes), **F4** (pinned rev
>   not enforced on re-install — a re-install now reproduces the locked commit;
>   `install --update` moves it).
> - **Still open:** F5 (tar entry-type handling — `read_tar` still special-cases only
>   `'L'`), F6 (CLI panic on non-UTF-8 arguments; exit-code contract), F7 (module-loader
>   symlink/walk-up), F8 (REPL). F9 was informational.
>
> Full status table: `assessment/AUDIT_2026-10-20.md` §9. Reproduce the
> dynamic checks with `python audit_notes/probes/verify_2026-10-21.py`.


Scope: `src/pkg/mod.rs`, `src/modules/mod.rs`, `src/sched/mod.rs`, `src/repl/mod.rs`, `src/cli.rs`.
Method: static code inspection only (no build/exec per task boundary). All line numbers from the
working tree at `C:\Users\needyamin\OneDrive\Desktop\GaliLang`, read 2026-10-20.
Evidence tiers used: **[T1]** primary code in this repo; **[T2]** official docs/README cross-reference;
**[T3]** external web sources (cited); **[T4]** inference, marked as such.

---

## 1. Conclusion summary

The README's ecosystem claims are **partially** verified. The tar pack/unpack path-traversal guard,
the hand-rolled SHA-256, and the module import-path guard are real and substantially correct.
The weakest points are: (1) git dependency URLs are passed to `git clone` with **no scheme
allowlist**, so a hostile `spider.toml` can execute arbitrary commands via git's `ext::` transport;
(2) the scheduler's blocking primitives consume fixed pool workers, creating a starvation/deadlock
class the docs only half-acknowledge; (3) the lockfile's SHA-256 covers lossy-UTF-8 *text*, not raw
bytes, and is never re-enforced after install. Overall confidence: high for findings 1–6 (direct
code evidence), medium for the wasm/Value-aliasing notes flagged as [T4].

---

## 2. Findings (ranked by severity × confidence × user impact)

### F1 — HIGH / confidence high — Git dependency URL accepts `ext::` (and any scheme) → arbitrary command execution from a hostile manifest

- **Fact.** `fetch_git` validates only that the URL does not start with `-`
  (`src/pkg/mod.rs:1065`), then shells out: `run_git(&["clone", "--quiet", "--", url,
  &path_str(&dir)])` (`src/pkg/mod.rs:1088`), executed by `std::process::Command::new("git")`
  (`src/pkg/mod.rs:1105-1112`). There is no scheme allowlist (`https://`, `ssh://`, `git://`,
  local path). Git's `ext::` transport treats the remainder of the URL as a command to run
  via the shell.
- **Impact.** Any project a user runs `spid3r install` in can gain code execution with a
  manifest line `extras = { git = "ext::sh -c <cmd>" }`. `rev` is charset-restricted
  (`src/pkg/mod.rs:1068-1080`) and the `--` separator prevents *option* injection — the code
  clearly thought about `-` smuggling (comment at `:1064`) — but stopped one step short.
- **External grounding [T3].** ext:: executes its argument as a command; the same root cause
  (attacker-controlled URL passed to `git clone`) is CVE-2022-25912 (simple-git) and is
  described at https://codeant.ai/blogs/exploiting-git%E2%80%99s-ext-protocol-for-command-execution
  ("When Git encounters an `ext::` transport, it launches the command using the system shell")
  and https://www.sentinelone.com/vulnerability-database/cve-2022-25912.
- **Evidence tier:** [T1] code path fully read; [T3] for the git behavior. Fix: allowlist
  `https://`/`ssh://`/`git://`/file paths, or pass `-c protocol.ext.allow=never` (git ≥2.38
  has `protocol.*.allow`).

### F2 — HIGH (correctness) / confidence high — Scheduler: blocking calls park fixed pool workers → starvation/deadlock; no shutdown, unbounded job queue

- **Fact.** Pool size is fixed once at first use, `available_parallelism()`
  (`src/sched/mod.rs:185-187`), workers loop forever over one queue
  (`src/sched/mod.rs:194-206`). Four Spid3r-visible operations park the *calling worker*:
  `ChannelData::recv` (`:40-47`), `MutexData::lock` (`:89-97`), `TaskHandle::join` (`:136-143`),
  `sleep_ms` (`:298-302`). A parked worker never picks up queued jobs.
- **Deadlock path [T1+T4].** If ≥ N tasks block simultaneously (N = pool size) — e.g. N tasks
  calling `recv()` on an empty channel, or an await chain N deep — all workers are parked and
  the ready queue (holding the tasks that would unblock them) never drains. The module doc
  documents "recv … blocks forever" and "await … consuming the joining worker"
  (`:6-10`) but not the pooled-starvation deadlock; `pool_size()` is not consulted anywhere to
  bound blocking.
- **Secondary.** (a) Job queue is unbounded with no backpressure — `spawn_job` push at
  `:277` (channels are documented unbounded at `:8-9`; the *job* queue is not mentioned).
  (b) No shutdown/join-on-exit API: workers are detached; when `main` returns they are killed
  mid-task — a task holding a half-written file at exit loses it (answers acceptance item (d):
  there is **no** join-on-exit semantics). (c) Worker spawn failure aborts the whole process:
  `.expect("failed to start a spid3r worker thread")` at `:208`, after reserving 32 MiB stack
  per worker (`WORKER_STACK_BYTES`, `:167`) — N×32 MiB virtual at startup.
  (d) `MutexData::unlock` (`:99-104`) has no ownership check — any task can release a mutex
  held by another; a re-entrant `lock` by the holder self-deadlocks (`:89-97`).
  (e) `MutexData::lock` returns a **clone** (`:97`) while the doc comment says "aliased"
  (`:81-83`); whether this is safe depends on `Value` internals (Arc-shared collections alias,
  PODs snapshot) — interpreter side is out of my scope; flagged [T4].
- **Mutex/channel poisoning:** all `lock().unwrap()` (`:35,41,49,53,57,91,99,139,195,243,256,
  274,277,284`) would panic if poisoned — but jobs run *outside* scheduler locks and job panics
  are caught by `catch_unwind` (`:261-273`), so self-poisoning is not reachable from user tasks.
  Cargo.toml has no `panic = "abort"` (checked), so unwind is in effect. This part is sound.

### F3 — MEDIUM / confidence high — Lockfile SHA-256 hashes lossy-UTF-8 text, not raw bytes; and only `*.matrix` files

- **Fact.** Both hash constructions push `String::from_utf8_lossy(&bytes)` into the hash input:
  `tree_hash` (`src/pkg/mod.rs:627`) and `vendor_dep` (`:915`). Two distinct files whose bytes
  differ only in invalid-UTF-8 sequences hash identically; the recorded `sha256` in
  `spider.lock` therefore does not uniquely pin file content, weakening "a SHA-256 hash for each
  dependency" (README.md:437). Contrast: `pack` hashes the raw archive bytes
  (`:651`, `sha256_hex(&data)`), so the two hash notions differ.
- **Second gap.** `collect_files` — used by both `tree_hash` and `vendor_dep` — filters
  `extension() == "matrix"` (`:1039-1048`), so tampering with any non-`.matrix` file inside a
  vendored tree is invisible to the tree hash. (`pack_entries` uses `collect_all_files`,
  `:583-610`, so pack covers everything — inconsistent coverage.)
- **Performance note.** The entire concatenated tree is built as one `String` then hashed
  (`:913-918`) — O(total tree bytes) memory per dependency.
- **Evidence tier:** [T1]; README claim [T2] at README.md:437.

### F4 — MEDIUM / confidence high — "Pinned to rev" is not enforced on re-install; registry checksum verification is opt-in and skipped silently

- **Fact (a).** `fetch_git` re-fetches and re-checks-out the *manifest* rev (or the branch tip,
  including `pull --ff-only`) on **every** install (`src/pkg/mod.rs:1082-1096`). `spider.lock`
  records the resolved commit (`origin = "git+url#commit"`, written at `:1278-1281` /
  `:1370`), but **no code path reads `spider.lock` back** to check out the locked commit or to
  verify `vendor/` against the locked SHA-256 — `install` recomputes everything from source
  (`:1259-1382`). Delete `.spid3r/git` and re-install: you get the new tip, not the locked
  commit. "git, pinned to a commit" (README.md:432) holds only while the manifest still names
  the rev and the manifest itself is unchanged; the lockfile is a record, not an enforcement
  mechanism (no `--frozen`/verify mode).
- **Fact (b).** Registry `sha256.txt` is verified only if present and readable:
  `if let Ok(expected) = std::fs::read_to_string(dep_dir.join("sha256.txt"))`
  (`:931-941`) — missing file → silent skip; and the declared hash lives *inside the untrusted
  package directory*, so it detects transit corruption, not malice. Comparison is
  case-sensitive after `trim()` (uppercase hashes fail with "mismatch").
- **Evidence tier:** [T1]; README claims [T2] at README.md:432,437.

### F5 — MEDIUM-LOW / confidence high — unpack traversal guard is sound; tar entry-type handling has gaps

- **Sound (verified, acceptance item b).** `unpack` (`src/pkg/mod.rs:699-733`) rejects entry
  names containing `..` anywhere, any `:` (kills Windows drive prefixes and ADS), and leading
  `/` or `\` (`:715-720`); then a lexical backstop `if !target.starts_with(dest)` (`:724`,
  component-wise on `Path`, so `dest2/…` cannot spoof `dest/…`). Extraction is files-only
  (`fs::write`, `:731`) — **no symlink/hardlink entries are ever created**, and the refusai to
  overwrite an existing destination (`:700-705`) prevents symlink-following into a prepared
  tree. Residual: the guard is over-broad (any name *containing* `..`, e.g. `a..b.txt`, is
  rejected — safe but lossy), and a classic TOCTOU remains if `dest`'s parent is a
  world-writable directory (attacker races a symlink under `dest` between `create_dir_all`
  and `write`) — not Windows-relevant.
- **Gaps.** `read_tar` ignores `typeflag` except `'L'` (`:523`, `:534`): directory entries
  (`'5'`) are written as zero-byte *files* (a following `src/main.matrix` entry then fails in
  `create_dir_all` with a confusing error), hardlink entries (`'1'`) become empty files, and
  pax extended headers (`'x'`/`'g'`) are unhandled — a GNU/pax-format archive with >100-byte
  names unpacks with **silently truncated** names instead of an error. The README's ustar
  claim (README.md:439-441) is accurate for self-produced archives (checksum validation at
  `:501-514`, ustar prefix + `@LongLink` both supported `:534-542`, `:458-478`); interop with
  *foreign* tar formats is not.
- **Evidence tier:** [T1]; README [T2].

### F6 — LOW-MEDIUM / confidence high — CLI: one real panic on hostile filesystem input; exit-code contract leaks

- **Panic.** `cmd_test` calls `f.as_os_str().to_str().unwrap()` three times
  (`src/cli.rs:598-602`); an `examples/*.matrix` filename that is not valid UTF-8 (trivial to
  create on Unix) panics the tool. Everything else in arg parsing is panic-free: every
  `args.get(i)` is bounds-checked with usage errors (`--engine` `:333-341`, `--break` `:763-770`,
  `--runs` `:936-944`), `files[0]` is length-guarded (`:539-545`), `--runs 0` rejected (`:941`),
  and `single_file` enforces arity. Acceptance item (e): **no panics from malformed CLI args
  found**; the one panic is filesystem-controlled.
- **Exit codes.** Doc contract is 0/1/2 (`src/cli.rs:4`, HELP text, README). Leaks: a panic
  anywhere in dispatch is re-raised via `resume_unwind` (`:104`) → process exit **101**; `debug`
  quit returns **3** (`:818`), documented only in a code comment (`:749-750`), not in HELP.
- **Cosmetic but user-visible:** two error strings contain a literal `//n` where `\n` was meant
  (`src/cli.rs:681`, `:685`) — differential-failure output prints as one mangled line.
- **Also:** `project_test_in` mutates process-global cwd (`std::env::set_current_dir`,
  `:698-703`) while the CLI already runs dispatch on a worker thread (`:95-101`) — safe today
  (single dispatch thread) but fragile if commands ever run concurrently.
- **Evidence tier:** [T1].

### F7 — LOW / confidence high — Module loader: guard is solid; symlink and vendor walk-up escape hatches remain

- **Sound (acceptance item f).** `check_import_path` rejects absolute paths, rooted paths, any
  `:` (drive letters, incl. `C:foo`), and any `..` component (`src/modules/mod.rs:59-75`); both
  resolution paths (semantic `public_names_of_in:287-291` and runtime `resolve:78-80`) call it.
  Imports cannot escape the search roots by spelling alone, on either platform.
- **Escape hatches.** (a) A symlink inside any search dir (incl. `vendor/<dep>/` shipped by a
  git dep) passes `c.is_file()` and `canonicalize` follows it *outside* the roots; there is no
  containment re-check after canonicalization (`:100-107`) — a dependency can make
  `import "x"` load/execute an arbitrary file readable by the user [T1+T4 on impact].
  (b) `find_vendor_dir` walks up from cwd to the filesystem root adopting any `vendor/`
  (`:260-271`) — running inside a subdirectory silently imports an ancestor project's vendored
  code (shadowing hazard). (c) `SPID3R_PATH` is split on `';'` *then* `split_paths`
  (`:229-233`) — a Unix entry containing a literal `;` is mangled (cosmetic).
- **Module cache/cycles:** keyed on canonicalized real path (`:100-107`, `:118-131`), cycle
  detection via `in_progress` chain (`:143-152`), join-repeatable exports; `lock().unwrap()` on
  the module mutex (`:141,145,195,198`) can panic if ever poisoned, but the guard is never held
  across user code execution — sound in practice.
- **Evidence tier:** [T1].

### F8 — LOW / confidence medium — REPL: multiline heuristic and stdout panics

- `balanced()` (`src/repl/mod.rs:88-134`) models only double-quoted strings, `//` and
  `/* */`. If the lexer has other string/char delimiters, nesting detection misfires; a grep
  for a `Char` token under `src/lexer*/` found nothing, but the lexer layout was not fully
  audited (timebox) — **unverified assumption**, tier [T4]. `depth <= 0` lets an unbalanced
  *closer* through to a parse error (benign). `print!("{}", prompt)` (`:22`) panics on a closed
  stdout (`spid3r repl | head -1` → exit 101 instead of clean shutdown). Good parts: bad lines
  are survived, `:reset` rebuilds the full session state including the const set
  (`:41-51`, `:81-86`), each line evaluates on a big-stack scoped thread with panic→diagnostic
  conversion (`:205-227`), and the bare-expression retry is parse-only so nothing can execute
  twice (`:178-196`). Note: spawned tasks from before `:reset` keep running on the shared
  global scheduler (`Scheduler::shared`, `src/sched/mod.rs:173-175`) — old-session tasks can
  still mutate shared state after a reset [T4].

### F9 — INFO — The uncommitted `#[cfg]` edit in sched does not change non-wasm behavior

- **Fact (acceptance follow-up).** The working-tree change gates `queue`/`cv` fields and all
  touching code with `#[cfg(not(target_arch = "wasm32"))]`: fields `src/sched/mod.rs:148-151`,
  init `:176-178`, pool spawn `:183-208`, spawn_job enqueue `:260-280`, `sleep_ms`/`now_ms`
  `:298-305`; the wasm path executes jobs inline with `catch_unwind` and stores the result
  synchronously (`:245-259`). On non-wasm, every branch compiled before still compiles
  identically — the edit only removes the fields on wasm. **Non-wasm behavior: unchanged**
  (verified by reading the full file; the only wasm-visible hazard is `ChannelData::recv`
  reaching `Condvar::wait`, which wasm32-unknown-unknown std does not support — would panic at
  runtime if a wasm program ever blocks on an empty channel; unreachable through `spawn_job`'s
  inline path, tier [T4]).

---

## 3. What is SOUND (verified, per acceptance item g)

- **SHA-256 is a real FIPS 180-4 implementation**, not a stub: full K-constant table
  (`src/pkg/mod.rs:741-750`) and round function (`:757-833`) check out against the standard;
  padding/big-endian length correct (`:765-769`); known vectors asserted in tests
  (`:1443-1457`: empty-string `e3b0c4…`, `"abc"`, 448-bit message) plus the bytes/hex identity
  test (`:1459-1476`). (Weakness is the lossy-UTF-8 *input* construction, F3, not the
  primitive.)
- **unpack path-traversal protection verified** against all four acceptance vectors — `..`
  segments (substring rejection), absolute paths (leading `/` and `\`), drive letters and
  ADS (`:` rejected anywhere), symlinks (never created; fresh dest enforced) + lexical
  `starts_with(dest)` backstop (F5, `src/pkg/mod.rs:715-724`).
- **ustar writer/reader self-consistency:** header checksum computed and validated
  (`:448-455`, `:501-514`), ustar prefix split ≤155/100 (`:405-420`), GNU `@LongLink` for
  longer names (`:458-478`, `:534-542`), deterministic (mtime 0, sorted entries).
- **Package-name validation** blocks `vendor/`/`.spid3r/git/` escapes incl. the historical
  `..`-key `remove_dir_all` footgun (`:856-874`, defense-in-depth at `:886-890`).
- **Import-path guard** for the module loader (F7, `src/modules/mod.rs:59-75`).
- **CLI arg parsing** is panic-free on malformed args (F6); scheduler mutex/channel poisoning
  is unreachable from user tasks (F2 end).

## 4. 待证项 (open leads)

- Whether `Value` collections are Arc-shared (decides F2(e) severity): read
  `src/value/mod.rs` `Value::Map/List` definitions and the interpreter's `mutex` set/get natives.
- Lexer string/char delimiter inventory for F8: read `src/lexer` token definitions.
- Whether the VM engine (out of scope, `src/vm`) honors the same import guard —
  `check_import_path` is called from `public_names_of_in` and `resolve`; confirm the VM's
  import path uses the same helpers.
- wasm32-unknown-unknown `Condvar::wait` behavior claim (F9) — verify against current std
  docs before asserting.

## 5. Suggested next steps (if investment continues)

1. Fix F1: scheme allowlist or `-c protocol.ext.allow=never` in `run_git` (one-line change,
   highest risk reduction).
2. F2: make `recv`/`join`/`sleep` resume the scheduler (park the *task*, not the worker), or at
   minimum detect "all workers blocked while queue non-empty" and abort with a diagnostic.
3. F3/F4: hash raw bytes; hash all files; add a `--frozen` mode that verifies `vendor/` against
   `spider.lock` and checks out the locked commit for git deps.
4. F5: handle typeflag `'5'` (mkdir), reject `'1'`/`'2'`/pax explicitly with a clear error.
5. F6: replace the three `to_str().unwrap()` with `to_string_lossy()`; document exit 3.

— End of report. Static inspection only; no code was executed or modified outside
`assessment/findings/`.
