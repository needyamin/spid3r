# 07 — Stdlib static inspection (globals + net + regex + csv + datetime)

> **Status 2026-10-21 (verification round).** This file is the 2026-10-20
> static record and is not rewritten. What changed since:
>
> - **Fixed:** **F1** (regex unbounded recursion aborted the host — audit A-2; now
>   `MAX_REGEX_DEPTH = 512` with a clean error).
> - **Still open, and re-measured on 2026-10-21:** **F2** (regex is O(n²·|NFA|) — audit
>   A-15: 2 500 chars took 634 ms, 5 000 chars took 2 553 ms, i.e. 4.0× for 2× input;
>   20 000 chars exceeds 30 s), **F6** (negated classes never match `\n` — reproduced),
>   **F5** (CSV BOM is not stripped — no BOM handling in `src/stdlib/csv.rs`), F3/F4
>   (`net.rs` buffer growth and the missing server read timeout — audit A-16, not
>   exercised), F7 (datetime validation), F8-F11 (smalls).
>
> Full status table: `assessment/AUDIT_2026-10-20.md` §9. Reproduce the
> dynamic checks with `python audit_notes/probes/verify_2026-10-21.py`.


Scope: `src/stdlib/mod.rs`, `src/stdlib/net.rs`, `src/stdlib/regex.rs`, `src/stdlib/csv.rs`, `src/stdlib/datetime.rs` (GaliLang / Spid3r).
Method: full read of all five files (mod.rs paged over two reads), plus one read-only verification of `src/pkg/mod.rs::sha256_bytes` because the stdlib crypto builtins delegate to it. No build/run (constraint); all findings are static structural evidence, marked with confidence.
Date: 2026-10-20. Claims in README were not trusted; each was checked against code.

---

## 1. Ranked summary

| # | Severity | Confidence | Area | One-line |
|---|----------|-----------|------|----------|
| F1 | High | High | regex | Unbounded recursion (parser + NFA closure) → stack-overflow abort of the host process |
| F2 | High | High | regex | Match loop restarts at every index: O(n²·\|NFA\|); "Linear time" doc claim false; CPU DoS on long inputs |
| F3 | Medium | High | net | Chunked-response reader never compacts its buffer; trailer loop uncapped → hostile server OOMs `http_fetch` client |
| F4 | Medium | High | net | Server-side `read_head` has no read timeout → slowloris parks worker threads indefinitely |
| F5 | Medium | High | csv | No BOM stripping; type inference conflates ""/NA/null→Null and writer maps Null→"" (lossy round-trip); one-field `""` row silently dropped |
| F6 | Medium | High | regex | Negated character classes can never match `\n` — silent deviation from PCRE/RE2 semantics |
| F7 | Low-Med | High | datetime | `datetime_from_parts` accepts Feb 30 / day 31 in short months, silently normalizes (round-trip asymmetry) |
| F8 | Low | Medium (conditional) | mod.rs (process) | Subprocess = argv-array, no shell (sound); residual: .bat/.cmd quoting only safe on Rust ≥ 1.77.2; no child-execution timeout |
| F9 | Low | High | regex | `[a-\d]` silently becomes range `('a','d')`; `[]` is an empty class, not literal `]` |
| F10 | Low | High | net | `http_fetch` treats any 1xx as terminal (no interim-response skip); a 100-continue server yields status 100 + empty body |
| F11 | Low | High | mod.rs | `abs(i64::MIN)` returns `i64::MIN` (`wrapping_abs`); user `server:` header can duplicate runtime's `server: spid3r` |
| F12 | Info | High | mod.rs/net | Remaining panic surface = poisoned-lock `unwrap()`s only; builtin dispatch validates arity/types before indexing |

---

## 2. Detailed findings

### F1 — Regex: unbounded recursion → process abort (High / High confidence)

Two independent unbounded recursions, both driven by user-supplied pattern text:

1. **Parser recursion.** `parse_atom` handles `(` by calling `parse_alt` again (src/stdlib/regex.rs:111-117). Chain `parse_alt → parse_concat → parse_piece → parse_atom → parse_alt` (regex.rs:57, 89, 106, 111) recurses once per nesting level with **no depth cap**. A pattern of ~10⁵ nested parens overflows the thread stack. Rust stack overflow is not a catchable panic — it aborts the process.
2. **Closure recursion in `add_state`.** `add_state` recurses through `Split` transitions (regex.rs:508, 511). `Alt` is compiled as a *chain* of splits, each new split's `next1` pointing at the previous fragment start (regex.rs:367-383), so closure depth = O(#alternation branches). `a?a?a?…` produces the same chain shape via the `Quest` arm feeding `Concat` (regex.rs:409-425, 348-365). A pattern like `(?:a|)` repeated ~10⁵ times (≈ a few hundred KB) reaches the same overflow.

Impact: any Spid3r program (or web-framework route handler compiling/running user-supplied regexes) can kill the whole host process, not just throw. The stdlib's own JSON parser treats exactly this hazard as a bug and guards it (`JSON_MAX_DEPTH = 512`, src/stdlib/mod.rs:1304, enforced in `parse_value`/`write_json`); regex has no equivalent. Fix direction: depth counter in `Parser` and iterative (worklist) `add_state`.

Note: `compile_node` (regex.rs:271) also recurses on AST nesting — same bound problem, third instance.

### F2 — Regex execution is O(n²·|NFA|), not linear (High / High confidence)

`regex_match`, `regex_find`, `regex_find_all`, `regex_replace` all restart the NFA simulation from **every** start index (regex.rs:547, 558, 572, 593 — `for start in 0..=chars.len()`), and each `match_from` call allocates a fresh `visited = vec![false; num_states]` (regex.rs:446) and runs until the active set dies. For patterns whose active set survives long spans (`.*x`, `[\\s\\S]*needle`), the work is quadratic in text length: a 100 KB body with a few hundred NFA states ≈ 10⁹-10¹⁰ state steps, plus one allocation per start index (10⁵ allocations per call).

The module doc claims "Linear time execution with no exponential backtracking" (regex.rs:6). Half of that is true and verified: the simulation is a genuine Thompson NFA — per position it walks a state *set* with a `visited` dedup (regex.rs:442-490, `visited.fill(false)` at 456), so there is **no exponential backtracking** (no ReDoS blowup like `(a+)+$`). But overall time is O(n²·s), not O(n). For a "pure-Spid3r web framework" regexing request paths/bodies this is a CPU-DoS lever. Fix direction: prefix prefilter (first-byte set), anchored-run fast path, or reuse one `visited` buffer across starts.

### F3 — net: chunked-response reader leaks its buffer; uncapped trailer loop (Medium / High confidence)

`ChunkReader::fill` appends network bytes to `self.buf` (src/stdlib/net.rs:472-480); `line()` and `take()` only advance `self.pos` (net.rs:482-503). **Nothing ever drains `buf[..pos]`**, so the reader retains every byte it has consumed. Additionally the post-body trailer loop

```rust
while !reader.line()?.is_empty() {}   // net.rs:450
```

is not counted against any cap: each individual line is bounded by `MAX_HEAD_BYTES` (net.rs:487-489), but the *number* of trailer lines is not, and since consumed lines are never freed, a hostile HTTP server can stream endless small trailer lines and grow client memory without bound. Impact: a Spid3r script calling `http_fetch` against a malicious URL can be OOM-killed. The chunk-*size* overflow path, by contrast, is correctly guarded with `checked_add` against `MAX_BODY_BYTES` (net.rs:455-461) and regression-tested (net.rs tests, `chunked_body_overflow_size_is_rejected`). Fix direction: `buf.drain(..pos)` (or an offset-compacting `VecDeque`) and fold trailer bytes into the 8 MiB cap.

### F4 — net: no server-side header-read timeout → slowloris (Medium / High confidence)

`http_read_request` → `read_head` blocks in `stream.read` (src/stdlib/net.rs:364-395) with no timeout. Timeouts are only installed on the *client* path of `http_fetch` (net.rs:763-764); server sockets default to blocking-forever unless the script opts in per handle via `tcp_set_timeout` (net.rs:325-343). Each blocked task occupies a worker thread (blocking-native model described at net.rs:5-9). A slowloris client dribbling 1 byte per interval holds a thread indefinitely; a handful of such connections stalls the interpreter's concurrency. Head *size* is correctly bounded (`MAX_HEAD_BYTES` 64 KiB, net.rs:29, 377-385), so this is purely the time dimension. Fix direction: default read timeout on accepted streams, or document that server code must set one.

### F5 — CSV: BOM + inference/round-trip surprises (Medium / High confidence)

Core RFC-4180 mechanics are sound: quote doubling, embedded CRLF inside quotes, CRLF/CR/LF row terminators, unclosed-quote error (src/stdlib/csv.rs:20-63). The gaps:

1. **No BOM handling.** `parse_csv_text` (csv.rs:9-19) never strips a leading UTF-8 BOM; `\u{feff}` falls into the literal branch (csv.rs:56-57) and becomes part of the first header name. Every Excel-exported file mis-keys its first column.
2. **Inference conflations** (`infer_csv_value`, csv.rs:102-121): `""`, `NA`, `null`, `NULL` all → `Null` (csv.rs:104) — empty string vs missing vs literal "NA" are indistinguishable; leading-zero ints degrade (`"00770"` → `770`, csv.rs:106); ints beyond i64 silently become `f64` (csv.rs:106-110) with precision loss; `"NaN"`/`"inf"` correctly stay strings (finite check, csv.rs:108-110) — that part is right.
3. **Lossy round-trip.** Writer maps `Null → ""` (csv.rs:124-127) and never quotes empty fields (csv.rs:128-135), so `NA`/`null` text and intentional `""` cells do not survive write→read: they all come back `Null`. Types of whole columns can shift across a round-trip.
4. **Single-field empty row dropped.** A row that is just `""`: quotes open (csv.rs:32-33) and close with `cur_field` still empty, then the `\n` branch sees `cur_row.is_empty() && cur_field.is_empty()` and *continues* (csv.rs:48-51) — the data row vanishes. Same trace for a lone `,`-less empty line is intended, but `""` is a legitimate one-column row.
5. **Writer hard-codes `','`** at both `format_csv_field` call sites inside `write_csv_file` (csv.rs:137-166) while the parser accepts a custom delimiter via `parse_csv` options (mod.rs:1487-1497) — `;`-delimited files cannot be round-tripped. Also `parse_csv`'s delimiter option silently takes only the first char of a multi-char string (mod.rs:1492-1495).

### F6 — Regex: negated classes never match `\n` (Medium / High confidence)

```rust
let ok = if *negated { !in_range && ch != '\n' } else { in_range };   // regex.rs:476
```

`[^x]` refuses to match a newline. POSIX/PCRE/RE2 all match `\n` with a negated class. Silent semantic deviation for multi-line text (`regex_match("[^x]", "\n")` → false). Positive classes are unaffected; `.` also excludes `\n` (regex.rs:459-465), which *is* standard.

### F7 — datetime: impossible dates accepted silently (Low-Medium / High confidence)

`datetime_from_parts` validates only `1..=31` for day (src/stdlib/datetime.rs:75-78) and delegates to `days_from_civil` (datetime.rs:30-44), so `(2025, 2, 30)` succeeds and normalizes to 2025-03-02; `second = 60` (leap-second style, allowed at datetime.rs:87-90) also normalizes to the next minute. Round-trip asymmetry: `datetime_parts(datetime_from_parts(y,2,30)) ≠ (y,2,30)`. Everything else checks out: the Hinnant civil-calendar algorithms are transcribed correctly (datetime.rs:9-49), epoch arithmetic uses `div_euclid`/`rem_euclid` so pre-1970 times are right (datetime.rs:56-58), weekday math verified by the epoch test (datetime.rs:133-145), and year range ±10⁸ with checked multiply/add (datetime.rs:66-70, 92-97). No timezone support at all — consistent with the "civil calendar" claim, but callers should note `format_datetime` can never emit local time.

### F8 — Subprocess: argv-array construction is sound; two residuals (Low / Medium, conditional)

`spawn_process` and `exec_capture` use `std::process::Command::new(cmd).args(argv)` with **no shell involvement** (src/stdlib/mod.rs:428-445, 462-530) — classic command injection does not apply; argument lists are strings-only and enforced (`argv_list`, mod.rs:~410-427). Residuals:
- On Windows, spawning a `.bat`/`.cmd` target historically allowed argument injection through std's quoting (CVE-2024-24576, fixed in Rust 1.77.2). Whether this crate is exposed depends on the toolchain version, which cannot be confirmed under the no-cargo constraint — conditional, unverifiable here.
- No execution timeout: a child that never exits parks the script forever (mod.rs:462-530); std has no `wait_timeout`, but the stdlib could poll `try_wait`.
Positives worth recording: both output pipes are capped at 64 MiB via `take(EXEC_CAPTURE_MAX_BYTES)` (mod.rs:454, 531-545), drained on worker threads so a full pipe cannot deadlock (mod.rs:476-505), the child is always reaped (mod.rs:515-526), and non-UTF-8 output is an explicit error rather than silent mojibake (mod.rs:539-545).

### F9 — Regex class-parser edge cases (Low / High confidence)

- `[a-\d]` silently builds range `('a','d')`: the range-end escape arm `Some(c) => c` (regex.rs:196-206) does not special-case `\d`/`\w`/`\s` the way the class-body escape does (regex.rs:168-186) — should be an error.
- `[]` closes immediately as an empty (never-matching) class and `[^]` as negated-empty (regex.rs:162-166); POSIX treats `[]` as a literal `]`. Minor deviation, but surprising.

### F10 — net: 1xx responses treated as terminal (Low / High confidence)

In `fetch`, `no_body` includes `(100..200)` (src/stdlib/net.rs:798) but there is no interim-response loop: a server that answers `100 Continue` (then the real response) makes `http_fetch` return status 100 with an empty body and close the socket. Rare in practice (the client never sends `Expect:`), but wrong per RFC 9110 §15.2. Everything else in response parsing is solid: status-line parse requires `HTTP/` prefix and numeric status (net.rs:790-797), `TE: chunked` correctly takes precedence over `content-length` (net.rs:803-806), missing content-length falls back to read-to-eof with the 8 MiB cap (net.rs:425-438), and HEAD/204/304 correctly skip the body (net.rs:798).

### F11 — mod.rs smaller items (Low / High confidence)

- `abs` on `i64::MIN` returns `i64::MIN` via `wrapping_abs` (src/stdlib/mod.rs:609) — a silent negative result from an "abs".
- `http_write_response`'s skip-list covers `content-length`/`connection`/`transfer-encoding` (net.rs:675-679) but not `server`, so a user-supplied `server:` header coexists with the runtime's `server: spid3r` (net.rs:692) — two `server` headers.
- `read_head` accepts heads up to `MAX_HEAD_BYTES + 8191` before rejecting (check runs before each read, net.rs:377-385) — bound holds, just imprecise.
- `connect_with_timeout` tries every resolved address serially at 10 s each (net.rs:212-224) — worst case unbounded total wait with a large DNS answer.
- Socket handles are never reclaimed except by explicit `tcp_close` (net.rs:99-104, 286-314) — scripts that loop-connect without closing leak table entries/fds.

### F12 — Panic audit of builtin dispatch (Info / High confidence) — largely SOUND

Checked every native in scope for `unwrap`/`expect`/direct indexing reachable with wrong arg types/counts: all validate arity first (`one`, `no_args`, `arity` helpers; net.rs:118-127) and types via `want_*` before indexing; `assert`/`assert_eq`/`parse_csv` options map (`m.find` before `entries[i]`, mod.rs:1478-1514), `write_csv` (mod.rs:1536-1567), regex/datetime wrappers (mod.rs:1569-1676), JSON parser (bounds-checked `get()` everywhere + `JSON_MAX_DEPTH = 512` guard, mod.rs:1304), `random_int` uses i128 span math so no overflow (mod.rs:1008-1018), `clamp` rejects NaN/inverted bounds before `f64::clamp`'s panicking path (mod.rs:~755-775). Remaining panic vectors are `lock().unwrap()` on *poisoned* mutexes only (net.rs:99, 159; csv.rs:151; mod.rs:1543, 1555) — reachable only after a panic elsewhere while holding the same lock; low practical risk. `SCRIPT_ARGS` RwLock unwraps likewise (mod.rs:389-398).

---

## 3. Soundness statement (what checked out)

- **Regex core**: genuinely a Thompson NFA with per-position state-set simulation and visited-dedup (regex.rs:442-490) — no catastrophic/exponential backtracking; zero-width matches advance correctly in `find_all`/`replace` (no infinite loops, regex.rs:567-605); longest-match semantics verified by construction (`Match` state records end during closure, regex.rs:529).
- **Crypto**: only SHA-256 + HMAC-SHA256 exist. HMAC follows RFC 2104 including >64-byte key pre-hashing (mod.rs:856-882); the delegated SHA-256 (src/pkg/mod.rs:757-824) is the standard FIPS 180-4 construction — correct IV, padding (mod-2⁶⁴ bit length via `wrapping_mul`, matching the spec), message schedule (σ/ROTR constants) and round functions; verified read-only. No red flags. Caveat: `random_*` builtins are a time/address-seeded xorshift (mod.rs:953-976) — fine for simulation, **not** a CSPRNG; nothing in the stdlib offers one.
- **HTTP framing (server)**: request line strictly requires 3 whitespace-separated parts + `HTTP/` prefix (net.rs:590-597, which also rejects bare-LF framing); chunked *request* bodies rejected outright (net.rs:596-601); duplicate `content-length` headers are folded into `"n, n"` by `read_head` (net.rs:398-406) and then fail the usize parse — smuggling via duplicate CL is dead; body capped at 8 MiB (net.rs:409-423).
- **HTTP framing (outbound)**: CRLF injection blocked twice — user header maps reject CR/LF and empty names (net.rs:159-176) and `http_fetch` rejects control chars in URL authority/path (net.rs:748-755); `http_write_response` strips user framing headers and always emits `content-length` + `connection: close` (net.rs:675-692); `http_fetch` likewise forces host/connection/content-length and rejects `transfer-encoding` (net.rs:767-780); method validated ASCII-alphabetic (net.rs:719-721); IPv6 authority handled (net.rs:740-746).
- **CSV core**: quote doubling, embedded newlines, CRLF/CR/LF handling, unclosed-quote error all correct (csv.rs:20-63); writer quoting condition covers delimiter/quote/CR/LF (csv.rs:128-135).
- **Datetime**: Hinnant algorithms correct; euclidean division handles negative epochs; tests cover epoch and round-trip (datetime.rs:127-152).
- **Dispatch**: no arg-count/type panic found in any builtin (see F12).

## 4. Out of scope / handoff notes

- `src/pkg/mod.rs::sha256_bytes` was read only to verify the crypto chain (sibling-owned; no issues found in it).
- `stdlib/http.matrix` (the pure-Spid3r routing framework) was not inspected — separate owner; F1/F2/F4 are directly relevant to it.
- `crate::sched::sleep_ms` is called with a raw i64 from `n_sleep_ms` (mod.rs:~335-345); negative-value behavior is sched/'s to own.
- `collections::len_of` / `construct` (mod.rs error paths) are value/-owned.

## 5. Suggested next steps (highest value first)

1. Add depth caps to the regex parser and make `add_state` iterative (F1) — smallest change, removes a process-kill.
2. Compact `ChunkReader` buffer + cap trailer bytes (F3); set a default accepted-socket read timeout (F4).
3. Strip BOM in `parse_csv_text` and give `parse_csv` a `typed: false` escape hatch plus writer delimiter passthrough (F5).
4. Fix `[^…]` newline semantics (F6) or document it loudly; align the "linear time" doc with an O(n²) worst-case note (F2).
5. Add month/day validation to `datetime_from_parts` (F7); decide a policy for `abs(i64::MIN)` (F11).
