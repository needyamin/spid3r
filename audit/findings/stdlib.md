# Audit — src/stdlib/mod.rs (Spid3r standard library)

- Date: 2026-10-18. Mode: READ-ONLY (no cargo run/build, no writes to src/ or tests/).
- Method: file read top-to-bottom; cross-checked against src/value/mod.rs, src/value/collections/mod.rs, src/interpreter/mod.rs, src/vm/mod.rs, src/native/rt.rs, src/cli.rs, src/sched/mod.rs and docs/STANDARD_LIBRARY.md. Every claim below cites file:line verified by grep/read on the current on-disk state.

## Snapshot anomaly (important, affects S1)

Two reads of src/stdlib/mod.rs during this audit returned different content for the file/process section only:

- Snapshot A (first read): `n_read_file` returned `Ok(Value::Str(...))` / threw `Err(format!("read_file '{}': {}", ...))`; `n_write_file` returned `Ok(()) => Ok(Value::Null)` / threw `Err(format!("write_file '{}': {}", ...))`; `n_spawn_process` returned `Ok(Value::Int(status.code()...))` / threw. This is exactly the behavior the user's probe reports (`write_file` → bare `null`, failures throw instead of `err(...)`).
- Snapshot B (current disk state, confirmed by a second read plus 4 independent greps): all three use `collections::construct("ok"/"err", ...)` and carry comments citing the documented contract (mod.rs:203-205, 224-225, 300-302). Greps for the Snapshot-A forms (`Ok(()) => Ok(Value::Null)`, `Err(e) => Err(format!("read_file...`) return zero matches on disk now.

Nothing outside lines ~201-313 differs between the snapshots (all statement-level grep anchors outside that range match). Nothing was modified by this audit. Most likely explanation: the probe ran against a stale build of Snapshot-A source; verification step: rebuild and re-run the probe. Root cause of the probe (as captured in Snapshot A): the natives returned the native fn's own `Result<Value, String>` — success = bare `Value::Null`/`Value::Str`, failure = `Err(String)`, and all three engines convert that `Err` into a thrown runtime error (src/interpreter/mod.rs:1022 `.map_err(|msg| self.rt(span, msg))`; src/vm/mod.rs:334-339 `VmError`; src/native/rt.rs:381-384 `rt_throw`). `read_file` had the same disease (returned `Value::Str`, threw on I/O error).

## Findings

### S1 [major] — read_file / write_file / spawn_process: contract now implemented, but probe proves a stale-build hazard

**Current code (verified):** `n_write_file` success returns `collections::construct("ok", vec![Value::Null])` (mod.rs:227), failure `construct("err", vec![Value::Str(...)])` (mod.rs:228-231). `n_read_file` mirrors it (mod.rs:207, 208-211). `construct` builds real `Value::Result` values (`"ok"` → `Value::Result(Ok(..))`, `"err"` → `Value::Result(Err(..))`, src/value/collections/mod.rs:64, 211-212). So current source satisfies docs/STANDARD_LIBRARY.md:24-25 ("returns `ok(text)` / `err(msg)`", "returns `ok(null)` / `err(msg)`").

**What the probe hit (Snapshot A, captured verbatim in this audit):** success arm was `Ok(()) => Ok(Value::Null)` and failures were `Err(format!(...))`. Root cause: the native's own `Result` was used as the language-level result; `Err(String)` is the engines' throw channel (src/interpreter/mod.rs:1022; src/vm/mod.rs:334-339; src/native/rt.rs:381-384), so failures aborted the program as runtime errors instead of yielding `err(msg)`. `read_file` was identical in kind (bare `Value::Str` on success, throw on I/O error).

**Repro (against a stale binary / Snapshot A):** `println(type_of(write_file("t.txt", "x")))` → `null` (docs: `result`); `let r = read_file("nope.txt")` → runtime error, `is_err(r)` never reachable. **Expected now:** `result`, `err("read_file 'nope.txt': ...")`.

**Fix direction:** already applied on disk; close the loop by rebuilding and re-running the probe, and add a regression test asserting `type_of(write_file(...)) == "result"` and `is_err(read_file("missing"))`. Remaining nuance: argument type errors (e.g. `write_file(1, "x")` via `want_str`, mod.rs:222) still throw, which is correct — only I/O failures are results.

### S2 [major] — `args()` returns the interpreter's argv, not the script's args

`n_args` reads the process argv directly: `std::env::args().skip(1)` (mod.rs:270-273). The CLI consumes argv[0] then dispatches on the subcommand (src/cli.rs:72-73) and `cmd_run` parses flags/script path in-process (src/cli.rs:161+), running via `run_tree` → `interp.run_program` (src/cli.rs:152-157) — no re-exec, no way for `n_args` to see a filtered vector. So `spid3r run app.sp --engine vm a b` yields `["run", "app.sp", "--engine", "vm", "a", "b"]`, contradicting docs:28 ("The argument vector passed to the program"). Every script that parses `args()` gets interpreter tokens first.

**Repro:** `spid3r run app.sp x` with `println(args())` → `["run", "app.sp", "x"]`.

**Fix direction:** pass the post-script argument slice explicitly from `cmd_run` into `Interp`/`install` (or set it once at startup) instead of reading `std::env::args()` inside the stdlib.

### S3 [major] — `random_int` is deterministic across runs, and panics on the full i64 range

RNG state is a thread-local seeded with a compile-time constant: `Cell::new(0x9E3779B97F4A7C15)` (mod.rs:466), xorshift at 469-481. The only reseed happens `if x == 0` from `now_ms()` (mod.rs:472-474) — unreachable, because a nonzero xorshift state never becomes 0. Consequences: (a) every program run emits the same sequence, so `random_int(0, 100)` returns the same first value every time (docs:48 promises "Random integer in `[lo, hi]`", with no seed API to opt into determinism); (b) every OS thread starts from the same constant, so concurrent tasks get correlated sequences.

Separately, `let span = (hi - lo + 1) as u64;` (mod.rs:511) overflows for wide ranges: `random_int(-9223372036854775808, 9223372036854775807)` panics (debug: subtract overflow; release: wraps to span 0 → remainder-by-zero panic at line 512). `% span` also introduces modulo bias for spans that do not divide 2^64.

**Repro:** `println(random_int(0, 100))` twice in separate runs → identical value; `random_int(i64 min, i64 max)` → panic.

**Fix direction:** seed per-thread from real entropy (or time × address × thread id) at first use; compute the range with `u128`/widening and use rejection sampling or a uniform-range algorithm; explicitly handle the full-range case.

### S4 [major] — `json_write` emits invalid / non-roundtrippable JSON for floats

`Value::Float(f) => out.push_str(&f.to_string())` (mod.rs:701) uses Rust's `Display`: NaN → `NaN`, ±inf → `inf`/`-inf` (invalid JSON tokens), integral floats → `2` (type degrades to int on reparse), and |f| ≥ ~1e19 → a pure digit string that exceeds i64. The parser then takes the i64 path (`text.parse::<i64>()`, mod.rs:605) and fails with `"json_parse: bad number"` — so the library cannot re-read its own output.

**Repro:** `json_parse(json_write(json_parse("1e30")))` → error `bad number`; `json_write(float("NaN"))` → `NaN` (invalid JSON); `type_of(json_parse(json_write(json_parse("2.0"))))` → `int`.

**Fix direction:** serialize floats with a JSON-safe formatter (shortest round-trip that keeps a `.0` for integral values, scientific notation for large magnitudes); reject or map non-finite values by policy (error is safest); in `parse_number`, fall back to `f64` when the i64 parse fails.

### S5 [major] — `json_parse` rejects valid `\uXXXX` escapes

`parse_string` handles only `n t r " \ /` (mod.rs:620-628); anything else returns `"json_parse: bad escape"` (mod.rs:629). Any real-world JSON containing `\u00e9` or surrogate pairs (`\ud83d\ude00`) fails to parse, even though `json_write` emits raw UTF-8 that its own parser accepts.

**Repro:** `json_parse("\"\\u0041\"")` → error `bad escape`; expected `"A"`.

**Fix direction:** decode `\uXXXX` (including surrogate-pair combining) into `char`s; keep the existing `bad escape` error for malformed escapes.

### S6 [major] — `json_write` deadlocks on self-referential containers; both JSON paths have unbounded recursion

The List arm holds `let d = d.lock().unwrap();` (mod.rs:717) while recursing into children (mod.rs:723); the Map arm does the same (mod.rs:728, 737). A list/map that (even indirectly) contains itself re-locks the same non-reentrant `Mutex` on the same thread → permanent hang, no error, no timeout. This codebase already documented the exact hazard: `values_equal` needed an identity fast path because `xs == xs` "locked the same non-reentrant Mutex twice and deadlocked" (src/value/mod.rs:555-556). Spid3r encourages back-edges via `weak`/`deref` (mod.rs:121-141), making self-referential structures likely.

Related, same fix family: `parse_value` (mod.rs:556) and `write_json` (mod.rs:696) recurse without a depth limit, so deeply nested input (`"[" * 100_000 + "]" * 100_000`) overflows the stack and aborts the process.

**Repro:** build a list containing itself (`let l = [1];` then append `l` to `l` via the language's list API), then `json_write(l)` → hangs forever. Deep nesting: `json_parse` of 100k nested arrays → stack overflow abort.

**Fix direction:** copy the child vector under the lock (drop the guard before recursing) or track a visited-identity set; add a depth cap (e.g. 512) returning `err`/throw with a clear message; same cap in `parse_value`/`parse_array`/`parse_object`.

### S7 [minor] — `json_write` map keys are not escaped, and non-string keys are silently stringified

Map arm writes `"` + `k.display()` + `"` (mod.rs:733-736). `MapKey::display` returns the raw string (src/value/mod.rs:68-74), so a key containing `"`, `\` or a control character produces invalid JSON (`{"a"b":1}`). Int/Bool/Null keys become quoted strings on output, so a parse→write→parse roundtrip changes their type (not visible from JSON alone, but Spid3r maps support non-string keys).

**Repro:** map with key `a"b` → `json_write` yields `{"a"b":1}`, which `json_parse` then rejects.

**Fix direction:** escape keys with the same string escaper used for values (factor it out), or error on non-string keys.

### S8 [major] — `spawn_process` mismatches its documented signature/return and blocks the worker thread

Code takes exactly 2 args `(cmd, list)`, runs `.status()` synchronously (mod.rs:303) and returns `ok(exit_code)` (mod.rs:304-307). Docs:36 say `spawn_process(cmd, args...)` and "returns a `task`". So: (a) callers following the docs cannot pass variadic args and get an int, not a task — `await`/task APIs won't work; (b) `.status()` blocks the calling OS thread until the child exits, stalling other green tasks scheduled on that worker; (c) `status.code().unwrap_or(-1)` conflates signal-termination with exit code −1 on Unix.

No injection/quoting risk: `Command::new(&cmd).args(&argv)` passes argv directly with no shell, so metacharacters in args are inert. Argument construction is also type-checked per element (mod.rs:285-299). This is the correct design; keep it.

**Repro:** `let t = spawn_process("sleep", ["5"]);` per docs → instead blocks 5s and yields `ok(0)`; docs' `t.await()` equivalent has nothing to await.

**Fix direction:** either implement the documented task-returning spawn (child on a task, `ok(exit_code)` resolved later) or fix docs to `spawn_process(cmd, [args])` returning `ok(exit_code)`; on Unix surface signal death distinctly (e.g. `ok(-signum)` or an err).

### S9 [minor] — `now()` is wall-clock, docs claim monotonic

`n_now` returns `sched::now_ms()` (mod.rs:256-258), implemented as `SystemTime::now().duration_since(UNIX_EPOCH)` (src/sched/mod.rs:246-251). docs:35 says "Millisecond monotonic timestamp". NTP/manual clock changes can move it backwards; pre-epoch clocks collapse to 0 via `unwrap_or(0)`. Timing code that subtracts two `now()` values can go negative.

**Fix direction:** back it with `Instant` (store a process-start `Instant` and offset), or correct the docs and add a separate monotonic accessor.

### S10 [minor] — `file_exists` returns false for directories

`Path::new(&path).is_file()` (mod.rs:235-237) is false for directories and other non-regular files, while docs:26 says "`true` if the path exists".

**Repro:** `file_exists("C:\\Windows")` → `false`.

**Fix direction:** use `Path::exists()` (or document the file-only semantics and rename).

### S11 [minor] — `int()`: truncation toward zero and silent saturation on float input

`Value::Float(f) => Value::Int(f.trunc() as i64)` (mod.rs:406): truncation is toward zero (`int(-3.7) == -3`, not floor); Rust `as` casts saturate, so `int(NaN) == 0`, `int(1e30) == i64::MAX`, `int(-1e30) == i64::MIN` — all silent. String path is solid: `trim()` then `parse::<i64>()` (mod.rs:408-412) → `" 42 "` → 42, `"+42"` → 42, `"0x2A"` → clean error, `""` → clean error `int(''): not an integer`.

**Fix direction:** pick and document a rounding rule (trunc is fine); return an error for non-finite/out-of-range floats instead of saturating.

### S12 [minor] — `float()` accepts `inf` / `NaN` / `infinity` spellings

`parse::<f64>()` (mod.rs:426-430) accepts Rust's special float strings, so `float("inf")` → ∞ and `float("NaN")` → NaN. Harmless on its own but feeds S4 (non-finite values then cannot be JSON-written) and is surprising for a conversion documented as "Convert to float". Whitespace/`+` handling mirrors `int()` (trim ok).

**Fix direction:** reject non-finite parses, or document them.

### S13 [minor] — numeric edge cases: `abs(i64::MIN)`, NaN in `min`/`max`, >2^53 precision, `pow` always float

- `abs`: `i.wrapping_abs()` (mod.rs:336) → `abs(-9223372036854775808)` is still negative (`i64::MIN`). Fix: error or return float.
- `min`/`max` (mod.rs:360-400): comparisons with NaN are always false, so `max(NaN, 1)` → `NaN`, but `max(1, NaN)` → `1` — result depends on argument order. Ties keep `args[0]` (`min(2, 2.0)` → Int 2). Mixed int/float compare via `as f64` (mod.rs:368-371, 388-391) loses precision above 2^53, e.g. `max(9007199254740993, 9007199254740992.0)` can select the float.
- `pow` (mod.rs:351-358) always returns `Value::Float` (even `pow(2, 10)` → `1024.0`) and converts int operands via `as f64` (mod.rs:85-86), losing precision above 2^53.

**Fix direction:** documented NaN policy (error or IEEE `maxNum`); compare int/float pairs in `i128`; consider int-preserving `pow` for int exponents ≥ 0.

### S14 [minor] — `eprint` appends a newline, unlike `print`

`n_eprint` uses `eprintln!` (mod.rs:178-181), so `eprint("x"); eprint("y")` yields two lines while `print` does not — the name implies print semantics and there is no `eprintln` builtin. Flushing is a non-issue: Rust's stderr is unbuffered. (`read_line` is correct per docs: EOF → `null` at mod.rs:188-190, strips `\r\n` at 190-192, I/O errors throw.)

**Fix direction:** use `eprint!` or add `eprintln` and make `eprint` match `print`.

### S15 [minor] — `bool(err(...))` is `true`; `len()` on option/result is odd

`n_bool` delegates to `truthy` (mod.rs:443-446; src/value/mod.rs:664-674). `Option(None)` is false, but `Result` falls into `_ => true`, so `if (err("boom"))` takes the true branch — a failed result reads as success in boolean context. `len()` (mod.rs:328-331 → src/value/collections/mod.rs:930-962) returns 1/0 for `some`/`none` (undocumented; docs:73 says "Length of a collection or string") and throws `"result has no length"` for results, `"int has no length"` for numbers/null.

**Fix direction:** decide result truthiness explicitly (err → false, or throw); document `len(option)` or remove it.

### S16 [minor] — `string()`/`print` list rendering contradicts docs

docs:80-82 says collections render like `list[1, 2, 3]`, `set{1, 2}`, `map{k: v}`. `display_value` renders Map/Set with prefixes but List as plain `[1, 2]` (src/value/mod.rs:736-744), and `string()` just forwards to it (mod.rs:438-441). Floats render `2.0` (good), NaN as `NaN`, strings unquoted (good for `string()`).

**Fix direction:** align the List arm with docs (`list[...]`) or fix the docs.

### S17 [minor] — `env()` is a third error convention: option, and it stringifies with a prefix

`n_env` returns `Option(Some(str))` / `Option(None)` (mod.rs:262-268). Missing variables are silent (fine), but `string(env("PATH"))` renders `some(C:\\...)` via `display_value` (src/value/mod.rs:920-924), so naive printing includes `some(...)`. The stdlib now mixes three conventions: result (files/process), option (env), throw (type errors). docs:27 only says "Read an environment variable".

**Fix direction:** document the option; consider `unwrap_or` ergonomics or a `result`-based variant for symmetry.

### S18 [minor] — `json_parse` accepts non-RFC numbers (leading zeros, trailing dot, leading dot)

`parse_number` (mod.rs:578-608) collects digits/`.`/exponent without grammar validation, then defers to Rust parsers: `007` → 7, `1.` → 1.0, `-.5` → -0.5 are accepted although invalid JSON; `1.2.3` is rejected only by the trailing-characters check (mod.rs:524-527), and `+1` is rejected at dispatch (mod.rs:556-570). Lenient parsing is usually harmless, but it means `json_parse` cannot be used to validate JSON.

**Fix direction:** validate the number grammar before parsing, or document leniency.

## Verified, no defect found (hunt coverage)

- **assert / assert_eq (hunt 6):** failures are thrown runtime errors (consistent with the rest of the stdlib; docs only promise results for file/process ops). Messages are exact and useful: `"assertion failed: {msg}"` / `"assertion failed"` (mod.rs:148-153), `"assertion failed: {actual} != {expected} — {extra}"` (mod.rs:164-172). Success returns `null`. Note `assert(0.0)`, `assert("")`, `assert(none)` fail (truthiness, src/value/mod.rs:664-674).
- **read_line (hunt 12):** EOF → `null`, `\r\n` stripped, I/O error throws (mod.rs:183-198); matches docs:20.
- **eprint flushing (hunt 12):** Rust stderr is unbuffered; `eprintln!` flushes per call. Only the newline semantics are off (S14).
- **spawn_process injection (hunt 11):** no shell is used (`Command::new(...).args(...)`, mod.rs:303); quoting/metacharacter injection is not possible; each list element is type-checked. Blocking/return-shape issues are in S8.
- **int whitespace/`+42`/hex/empty (hunt 2):** trim + `i64` parser handle `" 42 "` and `"+42"`; hex rejected; empty → clean error (mod.rs:408-412). Truncation direction and saturation are in S11.
- **type_of names (hunt 9):** delegate to `type_name` (mod.rs:448-451; src/value/mod.rs:676-722): `int`, `float`, `bool`, `string`, `null`, `list`, `vec`, `map`, `set`, `option`, `result`, `function`, `class`, `instance`, `task`… No conflict with docs ("int, list, …").
- **json_write on non-serializable values:** throws `"json_write: {type} cannot be written as JSON"` (mod.rs:743-749) — clear and correct.
- **json_parse trailing junk:** rejected explicitly (mod.rs:524-527).

## Suggested next step

1. Rebuild and re-run the `write_file` probe (S1) — the disk source already returns `ok(null)`; confirm the binary and add the regression test.
2. Highest user-visible risk after that: S2 (`args()`), S3 (deterministic RNG), S4/S6 (JSON write). S6 is a hang, so prioritize the cycle/depth guard.
3. If the Snapshot-A/Snapshot-B discrepancy is unexpected, check the working tree history (`git status`/`git diff`) to see whether the ok/err change was committed or is an uncommitted edit — this audit is read-only and did not touch the file.
