# Spid3r frontend static inspection — lexer, parser, ast, diagnostics, fmt

Date: 2026-10-20. Method: static reading only (no cargo/rustc/exec, per task boundary).
Files inspected (owned set): `src/lexer/mod.rs`, `src/lexer/token.rs`, `src/parser/mod.rs`,
`src/ast/mod.rs`, `src/diagnostics/mod.rs`, `src/fmt.rs`. Sibling files touched only by
bounded grep/read to confirm call contracts: `src/cli.rs` (fmt --check exit path),
`src/lib.rs` (public exports), `tests/fmt_tests.rs` + `fuzz/` (existence of enforcement).
rustc 1.98.1. All line numbers refer to the current working tree.

Legend: Severity high/med/low · Confidence high/med/low · Impact = user-visible consequence.

---

## 0. Verification of the established claims

| Claim | Verdict | Primary evidence |
|---|---|---|
| "Pratt parser (with recursion cap)" | VERIFIED — cap exists, value 96, enforced at 5 sites; coverage analysis in §2 | parser/mod.rs:8, 314, 462, 735, 773, 896 |
| "clean errors, never a crash" | VERIFIED for every path reachable from lexing/parsing user source; one latent panic exists only through public-API misuse (F1) | diagnostics/mod.rs:78-100; §1 |
| "-9223372036854775808 cannot be written as a direct integer literal" | VERIFIED, root cause located: `-` is lexed as a separate operator (next_token, lexer/mod.rs:153) and the magnitude literal `9223372036854775808` fails `parse::<i64>()` → hard lexer error | lexer/mod.rs:316-323, message at :321 |
| "formatter is canonical/idempotent" | PARTIALLY — idempotence holds by design and is tested over a corpus, but it is **not** enforced per-file in `format_source`; "canonical" is conditional (layout decisions are read back from source, so differently-laid-out equivalents format differently) | fmt.rs:5-8 (doc) vs fmt.rs:357-369 (only token preservation is runtime-checked); tests/fmt_tests.rs:2,36 |

---

## 1. Panic-path inventory (acceptance a)

Every failure path in the owned files returns `Result<_, Diagnostic>`; `Diagnostic` is a
plain value type (diagnostics/mod.rs:78-100) — no panics, no unwinds. Enumerated panic
candidates:

**Latent / conditional**

- **F1 · parser/mod.rs:28-29, 37-38 — unchecked token indexing; :32-33 — subtract-underflow.**
  ```rust
  fn kind(&self) -> &TokenKind { &self.toks[self.pos].kind }        // :28-29
  fn peek_kind(&self, n: usize) -> &TokenKind {
      let i = (self.pos + n).min(self.toks.len() - 1);              // :32-33
  ```
  With an **empty** `toks`, `self.toks.len() - 1` underflows (panic in debug; in release
  the `.min` keeps `pos + n` and `toks[pos+n]` panics on index OOB) and `kind()` panics at
  `toks[0]` first. Reachability: `pub fn parse(toks: Vec<Token>)` (parser/mod.rs:15) is
  exported via `pub mod parser` (lib.rs:15), so a library consumer (embed/wasm are
  sibling callers) can hit it with `parse(vec![])`. **Internal paths are safe**: the only
  token producer, `Lexer::tokenize_with_comments` (lexer/mod.rs:74-86), always pushes an
  `Eof` token, so `len >= 1` — the invariant is real but implicit and unenforced.
  Severity **med** (API contract, not user-reachable today) · Confidence **high** ·
  Impact: crash of the host process for a library caller, none for CLI users.

**Safe by construction (no reachable panic; listed because they are unguarded invariants)**

- lexer/mod.rs:296 `std::str::from_utf8(&self.src[begin..self.pos]).unwrap()` — the
  collected bytes are only ASCII digits/`.`/`e`/`E`/`+`/`-` (lex_number, :263-295), so the
  slice is always valid UTF-8.
- lexer/mod.rs:334 `String::from_utf8(self.src[begin..self.pos].to_vec()).unwrap()` —
  lex_ident (:329-333) collects only ASCII alphanumerics and `_`.
- lexer/mod.rs:391 `bytes.push(self.bump().unwrap())` — guarded by
  `matches!(self.peek(), Some(b) if b >= 0x80)` on the previous line (:389-390).
- Slices `&self.src[begin..self.pos]` (lexer/mod.rs:118, 143, 296, 334): `begin` is
  recorded before advancing and `pos` only moves forward, so `begin <= pos <= len` always.
- fmt.rs:329 `&f.src.as_bytes()[a..b]` — `a..b` are the lexer-produced byte ranges,
  parallel to the token vec by construction (lexer/mod.rs:74-86 pushes one range per
  token, including Eof).
- fmt.rs:451 `self.tokens[open + 1..close]` — `close` comes from the matching map, which
  only pairs `open` with a strictly later index; the `close <= open + 1` case is returned
  early (fmt.rs:444-445).
- fmt.rs:347-350 `INDENT.chars().next().unwrap()` — non-empty const (fmt.rs:38).
- Integer casts: span lengths `(self.pos - start_pos) as u32` (lexer/mod.rs:188, 247,
  297, 355) truncate only for source files ≥ 4 GiB — unreachable in practice; columns are
  `u32` (would need a 4-billion-column line).
- fmt unbalanced brackets: `)`/`]` without opener → `saturating_sub` + `if let Some(g)`
  guarded pop (fmt.rs:283-291); unclosed `(` at EOF → stale `wrap_stack` entry, no panic.
  Verified no panic path.

**Not found**: no `panic!`, `unreachable!`, `todo!`, array indexing, or integer-cast
panics reachable from user input in the six owned files (grep-swept).

---

## 2. Recursion cap (acceptance b) — VERIFIED

`const MAX_PARSE_DEPTH: usize = 96` (parser/mod.rs:8), enforced in:

- `parse_type` — parser/mod.rs:313-322 (generic `list<list<...>>` nesting)
- `parse_block` — parser/mod.rs:461-489 (all statement/block nesting funnels here)
- `parse_expr` — parser/mod.rs:734-745 (entry to every expression)
- `parse_assign` right-recursive RHS — parser/mod.rs:773-777
- `parse_unary` — parser/mod.rs:895-948 (`-----x`, `not not not ...`)

Coverage argument (static): every recursive cycle in the grammar passes through one of
the guarded functions. Expression nesting `((((...))))`, `f(g(h(...)))`, fn literals
(`parse_primary` → `parse_fn` → `parse_block`, parser/mod.rs:1017/249/461) and if/while/
try chains each cost ≥ 1 depth unit per level, so any nesting deeper than 96 produces the
clean error `"nesting deeper than 96 levels"` instead of unbounded recursion. Critically,
**postfix chains are iterative loops, not recursion**: `a[0][0][0]...`, `f()()()...`,
`a.b.c.d...` are handled in the `parse_postfix` loop (parser/mod.rs:950-1000) — the
classic stack-blower in recursive-descent parsers costs zero stack growth here. Left-
associative chains (`1+1+1+...`) are likewise loops (parse_or/and/cmp/add/mul,
parser/mod.rs:799-894). The `depth` counter is shared and restored symmetrically via the
closure pattern (`self.depth += 1; let res = (||...)(); self.depth -= 1;` — parse_type
:322-335, parse_block :489, parse_expr :743-745, parse_unary :944-947), so it measures
simultaneous nesting, not total work.

Residual risk (not a violation): one logical depth unit ≈ 10-12 Rust frames (parse_expr →
parse_assign → parse_or → and → cmp → add → mul → unary → postfix → primary [+ args]).
96 × ~11 ≈ ~1000 frames — safe on the 8 MB main thread even with debug-sized frames;
whether any frontend call runs on a small spawned stack is a sibling-file question, out of
scope. Severity **low** · Confidence **high** that the cap exists and covers all
recursion; **med** on the stack-budget margin.

Note: the cap is a *soft* limit — machine-generated but legitimate programs nested >96
deep get a clean "nesting deeper than 96 levels" error, not a crash. That is the intended
contract.

---

## 3. Lexer UTF-8 / BOM / escape handling (acceptance c)

**Sound:**
- UTF-8 BOM at file start is stripped (lexer/mod.rs:42-47: `bytes.starts_with(&[0xEF,
  0xBB, 0xBF])` → `pos = 3`); token byte ranges start after it, so raw-slice copying in
  fmt stays correct.
- Invalid UTF-8 inside a string literal → clean error `"invalid UTF-8 in string literal"`
  (lexer/mod.rs:393-400) — the multi-byte collector (:388-400) validates each char.
- Invalid bytes anywhere else → clean error via `from_utf8` check with per-char recovery
  for a valid multi-byte char (`"unexpected character '{}'"`, :236-241) or byte report
  (`"unexpected byte 0x{:02X}"`, :244). No lossy token content ever enters the AST.
- f64 overflow rejected: `1e999` → `"float ... is out of f64 range"` (:308-313); i64
  overflow rejected (:316-323).

**Gaps / findings:**

- **F2 · byte-based columns (med/high).** `bump()` increments `col` once per *byte* and
  resets it only on `\n` (lexer/mod.rs:88-98); `Span.len` is documented in bytes
  (diagnostics/mod.rs:11). Any line containing a multi-byte char (e.g. `s := "héllo" +
  x;`) over-counts columns for every token after it, so error carets on those lines point
  at the wrong character (rendering itself lives outside my files). No crash; diagnostics
  quality only. Impact: misleading error positions on non-ASCII source — a real concern
  for the intended non-English audience of a `.matrix` teaching language.
- **F4 · no unicode/hex/zero escapes (low/high).** The escape set is exactly
  `\n \r \t \\ \"` (lexer/mod.rs:369-384); `\u{...}`, `\x41`, `\0` all produce
  `"unknown escape '\u'"` (:383). Clean error, but users cannot express arbitrary
  codepoints or control characters — they must paste raw UTF-8. Capability gap, not a bug.
- **F7 · lone `\r` is not a line terminator (low/high).** `skip_trivia` treats `\r` as
  whitespace (:104-107) but only `\n` increments `line` (:95), so old-Mac/stray-CR files
  report wrong line numbers. Rare.
- Multi-line string literals are accepted silently; the token span points only at the
  opening quote (lexer/mod.rs:151-153 doc comment, 337-405) — intended per the doc, but
  any diagnostic inside such a string renders at the opening line.
- Comments are lexed with `from_utf8_lossy` (:118, 143): a comment containing invalid
  UTF-8 gets U+FFFD replacement chars, which the formatter will write to disk. Cosmetic;
  token preservation is unaffected (comments are not tokens).

---

## 4. Formatter: idempotency and the `fmt --check` contract (acceptance d)

**Architecture (verified):** `format_source` (fmt.rs:59) lexes but **never parses** — it
reprints the token stream with comments merged in source order (sorts at fmt.rs:76, 88).
Literals/identifiers are copied as raw source slices (fmt.rs:329), so `7.0`, string
escapes, and CRLF-in-strings survive byte-for-byte.

**Sound / verified:**
- **Token-preservation safety net**: output is re-lexed and compared token-kind by
  token-kind; any mismatch → `Err("fmt: internal error: formatted output changed the
  token stream; file left untouched")` (fmt.rs:357-369). Since the parser is a pure
  function of the token stream, token equality ⇒ identical AST ⇒ no semantic corruption
  is possible from formatting. This also catches spacing-rule hazards: e.g. input
  `a[1 . 5]` would re-space to `1.5` (Int Dot Int → Float), the net detects the change
  and refuses — safe failure, though the "internal error" wording is misleading for what
  is a lexer-level token-merge of invalid source.
- Unbalanced brackets cannot panic (see §1).
- `} else {` / `} catch {` joins (fmt.rs:136-150) work because the `must_break`
  consumption clears `pending_break` (fmt.rs:222-229) before the joined keyword is
  emitted; verified by trace.
- Wrap idempotency is structurally self-consistent: the flat-vs-wrap decision uses the
  **output** column (`col()`, fmt.rs:423-427) plus source read-back (`src_multiline`
  groups stay wrapped, fmt.rs:439-469), so pass 2 sees exactly the layout pass 1 wrote —
  a fixed point by induction. Not *proven* (no runtime check), but no counterexample
  found in static analysis of the decision sites (:469).
- **fmt --check exit-code contract is in code and matches the docs**: `cmd_fmt` counts
  per-file format errors and `changed` files; `errors > 0 → Ok(EXIT_USER_ERROR)`,
  `check && changed > 0 → Ok(EXIT_USER_ERROR)`, else `Ok(EXIT_OK)`; constants are
  `EXIT_USER_ERROR = 1`, `EXIT_OK = 0` (cli.rs:1095-1101, 18-19; usage text cli.rs:58-59
  and 1001-1002). A per-file format error does not abort the batch (cli.rs:1070-1075:
  prints, counts, continues) — good CI behavior: every file is still reported.
- Idempotence **is** enforced by test over the corpus: tests/fmt_tests.rs:2 ("must be
  idempotent, for every shipped example/benchmark and for hand-written cases"), failure
  message at :36. Fuzz targets for the frontend exist: fuzz/fuzz_targets/fuzz_lexer.rs
  and fuzz_parser.rs (presence verified; their contents are out of scope).

**Findings:**

- **F3 · the "enforced" wording overclaims runtime enforcement (low/high).** fmt.rs:5-8
  promises "Two guarantees are enforced for every formatted file", but `format_source`
  runtime-checks only token preservation (:357-369); idempotence is enforced solely by
  the external test corpus, not per invocation. Any future edit to the spacing/wrap
  rules that breaks idempotence on inputs outside the corpus ships silently. Suggested
  cheap hardening (one `assert`-style check in `format_source`): re-format the output
  once and compare, or at least fix the doc comment. Severity low because the corpus
  test exists today.
- **F10 · canonicality is conditional (info/low).** Layout decisions read back from the
  source: `}` and `else` on different source lines stay split (the join at fmt.rs:136-150
  only fires on stream-adjacent same-line pairs combined with source-line logic :226-233);
  a group already multiline stays multiline. So two equivalent files can format
  differently and `fmt --check` will fight about it — consistent with the module doc but
  worth knowing when the README says "canonical".
- **F5 · cosmetic spacing quirks that the token net rightly passes (low/high).**
  `ends_value` (fmt.rs:515-527) omits `RBrace` and `Minus`-itself, so `x - -y` prints as
  `x --y` (Minus/Plus arm fmt.rs:321-326, space rule :567-570) and `fn() {...} - 1`
  prints `}- 1`. Re-lexing yields identical token kinds (the lexer has no `--` token), so
  the safety net correctly allows it; purely cosmetic, stable on re-run.
- BOM is stripped silently by a fmt rewrite (lexer skips it; nothing re-emits it), and a
  CRLF file whose content is otherwise canonical is reported "unchanged" and left CRLF
  (cmd_fmt compares against an LF-normalized copy, cli.rs:1081-1086) — but a CRLF file
  that *does* need changes is rewritten with LF. Convergent (second run is a no-op),
  intentional-looking; noted for completeness.

---

## 5. Remaining lower-severity parser observations

- **F8 · `describe` embeds full string contents in parse errors**
  (parser/mod.rs:1092-1108, Str arm :1097 `format!("string \"{}\"", s)`): a huge or
  multi-line string token reproduces itself (raw, with newlines) inside the diagnostic.
  Bloat/misrendering only.
- **F9 · `fn new(...)` in statement position is mis-routed (low/high).** The Fn
  statement arm requires `peek_kind(1)` to be an `Ident` (parser/mod.rs:499); `new` is a
  keyword, so a nested `fn new(x) { }` inside a block falls through to the expression
  path, where `parse_fn` happily accepts `Kw(New)` as a name (parser/mod.rs:256-259) and
  the statement then demands `;`. Confusing errors for a plausible beginner mistake; no
  crash.
- `join()` (parser/mod.rs:1111-1123) builds composite spans with `saturating_add/sub` —
  no arithmetic panics; multi-line composites degrade to the start span's width by design
  (comment :1112-1116).

---

## 6. Bottom line

The frontend is in materially better shape than the audit brief anticipated: the
recursion cap is real and correctly placed, the "never a crash" claim survives a
panic-path sweep (the one latent panic, F1, requires public-API misuse with an empty
token vector, which the sole internal producer cannot emit), and the formatter's
token-preservation net makes semantic corruption structurally impossible. The genuine
engineering concerns are diagnostics ergonomics on non-ASCII source (F2 — the one I would
fix first), the API hardening of `parse` (F1), and honesty-of-documentation items F3/F10.

Ranked: F1 (med/high) · F2 (med/high) · F3 (low/high) · F4 (low/high) · F5 (low/high).
