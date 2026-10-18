# Lexer + Parser Audit — src/lexer/mod.rs, src/parser/mod.rs

Scope: READ-ONLY audit of `src/lexer/mod.rs` (13 KB) and `src/parser/mod.rs` (35 KB), Spid3r language,
repo `GaliLang`. Reference spec: `docs/GRAMMAR.md` (frozen Phase-0 grammar). No cargo runs; no source
edits. Line numbers are from the working tree as of 2026-10-18.

Known issues deliberately NOT re-reported: (a) UTF-8 BOM rejected as "unexpected character";
(b) local `fn` declaration needs trailing `;` (existence known — exact grammar rule documented at P7 below).

Severity scale: critical = wrong code accepted / host crash on ordinary input; major = crash on
plausible input, or spec violation with confusing failure; minor = diagnostic quality, portability,
unsupported-but-deterministic syntax.

---

## Part 1 — Lexer (src/lexer/mod.rs)

### L1 [major] u32 column underflow → debug panic / garbage span on `\` + newline inside a string
- **Where:** `src/lexer/mod.rs:336` — `Span::new(self.file.clone(), self.line, self.col - 2, 2)` in the
  unknown-escape error path of `lex_string`.
- **What breaks:** The escape byte is taken via `peek()` then consumed with `bump()` (mod.rs:325-331).
  If the byte after `\` is a raw `\n`, `bump()` (mod.rs:85-92) resets `self.col = 1`. The error branch
  then evaluates `self.col - 2` = `1 - 2` on a `u32`: in a debug build this panics
  ("attempt to subtract with overflow") — the user sees a Rust panic instead of a diagnostic; in a
  release build it wraps to ~4,294,967,294 and the reported column is nonsense. Reachable through the
  ordinary CLI path since any string ending a line with `\` hits it.
- **Repro (Spid3r):**
  ```
  let s = "abc\
  def";
  ```
  (backslash as last byte of the line, real newline after it).
- **Fix direction:** Record the backslash's own `(line, col)` before consuming the escape byte and use
  that for the error span; or compute the span from saved positions instead of `self.col - 2`
  (`saturating_sub` alone would hide the off-by-one, not fix it). Also decide the intended semantics:
  `\`+newline could be reported as "unterminated string" or defined as a line continuation.

### L2 [minor] No exponent floats: `1.5e3` mis-lexes as Float + Ident
- **Where:** `src/lexer/mod.rs:246-262` (`lex_number` has no `e`/`E` branch anywhere).
- **What breaks:** `1.5e3` → `Float(1.5)`, `Ident("e3")`; `1e5` → `Int(1)`, `Ident("e5")`. The parser
  then emits an off-target error (roughly "expected `;`" at `e3`) instead of anything mentioning the
  number. Deterministic, no mis-lex into wrong values.
- **Repro:** `let x = 1.5e3;`
- **Fix direction:** Lex `[eE][+-]?[0-9]+` as a float suffix in `lex_number` (after the fraction), or
  reject `e` adjacent to a number with a dedicated "scientific notation not supported" hint. Note
  `docs/GRAMMAR.md` defines neither `intLit` nor `floatLit` syntax, so the doc gap should be closed too.

### L3 [minor] `1.`, `1..2`, `.5`, `1.5.2` lex deterministically but produce confusing downstream errors
- **Where:** `src/lexer/mod.rs:252` — the `.` is consumed only when followed by a digit
  (`peek_at(1)` digit check); `src/lexer/mod.rs:229` — bare `.` is always `Dot`.
- **What breaks:** `1.` → `Int(1)` + `Dot`; `1..2` → `Int(1) Dot Dot Int(2)`; `.5` → `Dot Int(5)`;
  `1.5.2` → `Float(1.5) Dot Int(2)`. No token confusion, but the parser error will point at the `Dot`
  (e.g. "expected identifier after `.`") rather than the malformed literal. Note the digit-lookahead
  has the pleasant side effect that `1.foo()` and `1.5.foo()` method-call chains lex correctly.
- **Repro:** `let x = 1..2;`
- **Fix direction:** Optional dedicated lexer error for `..` ("range operator not supported") and for a
  `.` immediately after a number's `.`-less digits; otherwise document as-is.

### L4 [minor] Raw newlines allowed inside string literals; CRLF files change string values; span crosses lines
- **Where:** `src/lexer/mod.rs:293-360` (`lex_string` has no check rejecting `\n` or `\r` inside the
  literal); span built at `src/lexer/mod.rs:341-346` points at the opening quote with `len` = full byte
  length.
- **What breaks:** (1) A string containing a real newline is one token — the file-level comment at
  mod.rs:144 ("All tokens start and end on one line except strings with escapes") is false for these,
  and any one-line caret rendering of that span misdraws. (2) Portability: the raw line terminator is
  embedded verbatim in `TokenKind::Str`, so the same logical program lexes to a string containing
  `\r\n` on Windows and `\n` on Unix — values differ per platform. (3) Byte-level columns: `bump()`
  (mod.rs:85-92) counts bytes, so multi-byte chars inside the string inflate the columns of later
  tokens on the same line (diagnostics only).
- **Repro:** save with CRLF endings:
  ```
  let s = "line1
  line2";
  ```
  `s` now contains a literal `\r\n`.
- **Fix direction:** Either reject raw newlines in strings (force `\n` escapes) or bless multi-line
  strings and normalize line endings inside them; fix the mod.rs:144 comment either way. Consider
  counting columns in chars, not bytes.

### L5 [minor] Escapes limited to `\n \t \\ \"`; no `\r`, `\0`, `\u{...}`; NUL passes silently in content
- **Where:** `src/lexer/mod.rs:330-339` (only `n`, `t`, `\\`, `"` handled; everything else errors);
  content path mod.rs:342-356 accepts any valid UTF-8, including NUL.
- **What breaks:** `\u` gives "unknown escape '\u'" (no suggestion), and carriage return, NUL, and
  non-BMP unicode cannot be written via escapes — they can only appear raw (raw NUL inside a string is
  silently accepted). Outside strings, NUL is just "unexpected character '\0'" (mod.rs:222) — handled,
  no crash. `docs/GRAMMAR.md` does not specify string-literal escape syntax, so this is a spec gap
  rather than a code/doc conflict.
- **Repro:** `let s = "a\u{1F600}b";` → lexer error.
- **Fix direction:** Add `\r`, `\0`, `\u{...}` (and document the set in GRAMMAR.md), or improve the
  unknown-escape message to list the supported set.

### L6 [minor] Block comments do not nest; nesting yields downstream parse errors, not a clear message
- **Where:** `src/lexer/mod.rs:115-136` — scan terminates at the first `*/` with no depth counter.
- **What breaks:** `/* a /* b */ c */` closes at the inner `*/`; the residue `c */` becomes
  `Ident("c") Star Slash`, producing a parse error far from the real mistake. Unterminated block
  comments ARE handled cleanly (error at the opening `/*`, mod.rs:130-135) — no finding there.
- **Repro:** `/* /* nested */ still comment */ let x = 1;`
- **Fix direction:** Depth counter in the scan loop, or an explicit "nested block comments not
  supported" note in GRAMMAR.md.

### L7 [minor] `\r` retained in line-comment text (CRLF) — formatter-facing
- **Where:** `src/lexer/mod.rs:103-112` — the line-comment scan breaks only on `\n`, so the trailing
  `\r` of a CRLF file is bumped into `Comment.text`.
- **What breaks:** `spid3r fmt` (consumer of `Comment`, per mod.rs:35-38) receives comments ending in
  `\r`; re-emitting them yields mixed line endings or stray `\r` in output. Same applies inside
  multi-line block comments (mod.rs:115-136).
- **Repro:** `// hi<CRLF>let x = 1;` → `Comment.text == "// hi\r"`.
- **Fix direction:** Trim a trailing `\r` when storing the comment text (keep it only in the raw range).

### L8 [minor] Identifier rule is ASCII-only; non-ASCII error message mis-renders UTF-8
- **Where:** `src/lexer/mod.rs:285-291` (`is_ascii_alphanumeric || '_'`); message at mod.rs:222
  `format!("unexpected character '{}'", c as char)`.
- **What breaks:** `let café = 1;` errors at `c3` with a Latin-1 mojibake rendering (`'Ã'`) because one
  UTF-8 byte is cast via `as char`; each subsequent byte errors separately. Underscore-leading
  identifiers and bare `_` are fine (keyword table, src/lexer/token.rs:53-89, does not claim `_`).
  Likely an intentional ASCII policy — the finding is the misleading message, plus that GRAMMAR.md
  never states the identifier charset.
- **Repro:** `let café = 1;`
- **Fix direction:** Decode the full char (retry `std::str::from_utf8` on the remaining slice) for the
  message; document ASCII-only identifiers in GRAMMAR.md (or support XID).

### L9 [minor] Overflowing float literals silently become `Float(inf)`
- **Where:** `src/lexer/mod.rs:264` — `text.parse::<f64>()`; Rust's f64 parse returns `Ok(∞)` on
  overflow, so the `map_err` "bad float" branch (mod.rs:265-268) is in fact unreachable for the shapes
  `lex_number` can produce.
- **What breaks:** `let x = 111…1.5;` (hundreds of digits) lexes to `Float(inf)` and the program
  compiles with a silent infinity value; integer overflow by contrast is a clean error (mod.rs:270-280).
- **Repro:** `let x = 111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111.5;`
- **Fix direction:** Check `v.is_finite()` after parse and error like the i64 path does.

### L10 [minor] `-9223372036854775808` is unrepresentable (literal lexed positive)
- **Where:** `src/lexer/mod.rs:172` + `src/lexer/mod.rs:270-280` — the sign is a separate `Minus`
  token; the literal alone must fit i64.
- **What breaks:** `let x = -9223372036854775808;` fails at lex time with "integer '9223372036854775808'
  is out of i64 range" even though the intended value fits.
- **Repro:** as above.
- **Fix direction:** Accept `i64::MIN` as a special literal or fold sign into constant literal parsing.

### Verified-OK (checked, no finding)
- **Maximal munch:** all two-char operators (`==`, `!=`, `<=`, `>=`, `+=`, `-=`, `*=`, `/=`, `%=`, `->`)
  via `two()` (mod.rs:237-244) match longest-first; `===` lexes `==` + `=` (parser rejects); bare `!`
  is a clean error with a hint (mod.rs:213-221). `//` and `/*` are claimed by trivia before the
  operator arm, so division after trivia cannot be mis-eaten (skip_trivia loops, mod.rs:97-143).
- **Unterminated strings:** clean error at the opening quote (mod.rs:301-306), including `\` at EOF
  (mod.rs:318-325).
- **CRLF line numbers:** `\r\n` — `\r` bumps col, `\n` resets to line+1/col 1 (mod.rs:85-92, trivia
  mod.rs:100): no off-by-one on Windows files. Lone `\r` (classic Mac) is treated as a space, not a
  newline — nit only. Tab counts as 1 column (caret alignment in editors differs) — nit.
- **NUL bytes:** no crash; rejected outside strings (mod.rs:222), accepted as content inside strings.
- **Deeply nested input:** the lexer is fully iterative (no recursion anywhere in mod.rs) — no stack
  overflow risk at lex time.
- **Token/range bookkeeping:** `last_tok_start`/`ranges` are set after trivia (mod.rs:151-155), so
  formatter ranges exclude leading whitespace/comments as intended.

---

## Part 2 — Parser (src/parser/mod.rs)

### Precedence verification vs docs/GRAMMAR.md (rows 1–8, tightest first)

| Row | Grammar | Code | Verdict |
|---|---|---|---|
| 1 postfix: call/index/field/method | `postfix = primary { call \| index \| field \| method }` | `parse_postfix` mod.rs:866-916, loop over `(`/`[`/`.`; field vs method decided by `(` lookahead mod.rs:886-907 | ✓ match |
| 2 unary `not`, `-` | `unary = "not" unary \| "-" unary \| postfix` | `parse_unary` mod.rs:823-864, recursive operand | ✓ match (plus undocumented `await`/`spawn` arms → P2/P3) |
| 3 `*` `/` `%` | left-assoc loop | `parse_mul` mod.rs:801-821 | ✓ |
| 4 `+` `-` | left-assoc loop | `parse_add` mod.rs:780-799 | ✓ |
| 5 comparisons | `cmpExpr = addExpr { cmpOp addExpr }` | `parse_cmp` mod.rs:755-778, left-assoc loop | ✓ per grammar; chains accepted → P13 |
| 6 `and` | left-assoc | `parse_and` mod.rs:739-753 | ✓ |
| 7 `or` | left-assoc | `parse_or` mod.rs:723-737 | ✓ |
| 8 assignment, right-assoc, lowest | `assignment = target assignOp expr \| ...` | `parse_assign` mod.rs:686-721; RHS = recursive `parse_assign` → right-assoc ✓; all 6 ops (incl. `%=`) present | ✓ shape, targets → P4 |

Deviation from the doc: `await` is implemented at unary strength (mod.rs:825-829), not at the EBNF's
assignment row — see P3. There is **no pow-like operator at all** (token set src/lexer/token.rs:42-66
has no `**`/`^`), so "unary minus vs pow" is N/A; `-x % y` correctly parses `(-x) % y` (unary tighter
than `%` per rows 2–3).

### P1 [major] No recursion depth limit → host stack overflow on deeply nested source
- **Where:** every recursive descent chain — `parse_expr → parse_assign → parse_or → parse_and →
  parse_cmp → parse_add → parse_mul → parse_unary → parse_postfix → parse_primary` (mod.rs:682-1003);
  each `(` costs ~8 stack frames via the LParen arm (mod.rs:997-1002 calls `parse_expr`). Also
  unbounded: nested blocks (`parse_block` mod.rs:436 → `parse_statement` mod.rs:458 → `parse_block`),
  unary chains (`------x` — one `parse_unary` frame per `-`/`not`, mod.rs:840-864), nested lists
  (`[[[[…]]]]`), nested calls (`f(f(f(…)))` via `parse_args` mod.rs:918).
- **What breaks:** There is no depth counter anywhere (contrast rustc's recursion limit). A file with
  a few tens of thousands of nested parens/unary minus signs overflows the host thread stack →
  "thread 'main' has overflowed its stack" and the process aborts — a compiler crash (and a DoS vector
  if sources are untrusted), not a diagnostic.
- **Repro (Spid3r):** `let x = ((((……1……))))` with ~100 000 `(`; or ~100 000 `-` before `1`.
- **Fix direction:** Thread a `depth: u32` through `parse_expr`/`parse_unary`/`parse_primary`/
  `parse_block`, error with a clean "expression nesting too deep" diagnostic past a cap (≈128–512),
  or run the parser on a growable stack.

### P2 [major] `spawn` accepts only literal `fn` expressions; named calls/variables are rejected — contradicts the grammar
- **Where:** statement arm mod.rs:495-506 (`if matches!(callable, Expr::Fn(_))` at 501, error at 504);
  expression-level twin mod.rs:831-840 (check at 835).
- **What breaks:** `spawnStmt = "spawn" expr ";" ;  (* expr must evaluate to a function *)`
  (docs/GRAMMAR.md, statement section) states a *semantic* condition. The parser instead enforces a
  *syntactic* one: the operand must be an `Expr::Fn` literal. Every doc-conformant program whose
  operand merely *evaluates to* a function is rejected:
  `spawn work();` (a call), `spawn work;` (a fn-valued variable), `spawn self.run;` (field) — all fail
  with `spawn expects a function: spawn fn() { ... };`. The only accepted forms are `spawn fn() {…};`
  and ( undocumented) `spawn fn name() {…};` / `let h = spawn fn() {…};`.
- **Repro (Spid3r):**
  ```
  fn work() { }
  spawn work();
  ```
- **Fix direction:** Either drop the syntactic check and let the semantic pass decide (as the grammar
  says), or update GRAMMAR.md to `spawnStmt = "spawn" fnLit ";"` and mention it in the language docs.

### P3 [minor] `await` placement: code follows the doc's prose examples but contradicts the doc's own EBNF row; expression-position `spawn` is undocumented
- **Where:** `parse_unary` await arm mod.rs:825-829 (operand = `parse_unary`, i.e. unary strength);
  spawn arm mod.rs:831-840.
- **What breaks:** docs/GRAMMAR.md is internally inconsistent. The EBNF puts await at assignment
  level (`assignment = target assignOp expr \| "await" expr \| orExpr`), which would make
  `await t + 1` parse as `await (t + 1)`; the prose note says "await binds at unary strength" and
  "`await t.field` parses as `await (t.field)`" — which cannot both follow from the EBNF. The code
  implements the prose examples: `await t.field` → `Await(Field(t, field))` ✓ and `await t + 1` →
  `(await t) + 1` (differs from the EBNF row). Additionally, `spawn` is accepted as a *unary
  expression* (`let h = spawn fn() {…};`, mod.rs:831-840) although the grammar defines spawn only as a
  statement (`spawnStmt`) and `fnLit`-style productions list no `spawn` in `expr`.
- **Repro:** `let a = await t + 1;` and `let h = spawn fn() { };` — both parse; neither matches the
  frozen EBNF.
- **Fix direction:** Fix the EBNF row to `assignment = target assignOp expr \| orExpr` plus
  `unary = "not" unary \| "-" unary \| "await" unary \| "spawn" unary \| postfix`, and add the
  expression-position spawn to the doc (or remove it from the code).

### P4 [minor] Assignment-target check is shape-only, faithful to a loose grammar
- **Where:** mod.rs:699-707 — `target_is_ok = matches!(&left, Expr::Ident(_,_)/Expr::Index{..}/Expr::Field{..})`.
- **What breaks:** Any expression whose *outermost* node is Index/Field passes, because GRAMMAR's
  `target = ident \| postfix "[" expr "]" \| postfix "." ident` and `postfix = primary {…}` with
  `primary` containing `( expr )` allow arbitrary bases. So `(a + b)[0] = 1`, `f().x = 1`,
  `[1, 2][0] = 9`, `(x.y).z = 2` all parse and are left to the semantic pass to reject; the error
  message mentions only "a variable or an index", never field targets. Index/field assignment with all
  6 compound ops (`a[i] += 1`, `o.f *= 2`) is accepted, per the grammar note that only `list/vec/
  matrix/hash_table` may be index-assigned (semantic pass's job).
- **Repro:** `let x = (1 + 2)[0] = 5;` parses (target check passes); rejection depends on sema.
- **Fix direction:** Keep as-is if sema rejects; otherwise tighten the check to bases rooted in
  Ident/SelfKw and improve the message to "variable, index, or field".

### P5 [minor] `try` without `catch`/`finally` rejected — code stricter than the frozen grammar
- **Where:** mod.rs:655-666 (`if catch.is_none() && finally.is_none() { Err("try needs a catch or a
  finally block") }`).
- **What breaks:** `tryStmt = "try" block [ "catch" "(" ident ")" block ] [ "finally" block ]` — both
  clauses optional, so `try { … }` is grammatical per the doc but is a hard parse error in the code.
- **Repro:** `try { let x = 1; }` → parse error.
- **Fix direction:** Accept bare `try` (grammar-faithful) or amend GRAMMAR.md to require one clause.

### P6 [minor] Span bookkeeping bugs in `try`/`if` nodes (diagnostics point at the wrong region)
- **Where:** (a) `CatchClause.span = sp.clone()` mod.rs:646-651, where `sp` is the **`try` keyword's**
  span (mod.rs:640) — every catch clause reports its span as the `try` keyword. (b) `Stmt::Try` span
  mod.rs:667-671: `end_span` = finally block or **try body** — a `try {} catch (e) {}` without finally
  gets a span that excludes the entire catch clause. (c) `Stmt::If` span mod.rs:589:
  `join(sp, els.unwrap_or(&then))` — an `if/elif` chain without `else` excludes all elif bodies.
- **What breaks:** Error/caret rendering and formatter ranges under-cover the real construct; only
  diagnostics/format are affected, never semantics.
- **Repro:** `try { a(); } catch (e) { b(); }` — the try-stmt span ends at the try body's `}`.
- **Fix direction:** Track the true end (catch/finally block end, last elif/else block end) and use it
  in `join`.

### P7 [minor] Exact grammar rule for the known "local `fn` needs `;`" case (documentation of the mechanism, per task)
- **Where:** `parse_statement` mod.rs:458-517 has **no `fn`/`async` arm** — a statement starting with
  `fn` falls to the `_` fallback (mod.rs:511-517) → `parse_expr` → `parse_primary`'s
  `Kw(Fn) | Kw(Async)` arm mod.rs:976-978 → `parse_fn(true, None)` (anonymous fn-lit production,
  allow_anon) → back in the fallback: `expect(Semi, "';' after expression")` mod.rs:512.
- **The rule, precisely:** GRAMMAR.md's `statement` production contains no fn alternative — `fnDecl`
  exists only at `declaration` (program) level and as `methodDecl` in class bodies; `fnLit` is only a
  `primary`. So a `fn` at statement position *must* take the fn-literal path, and `exprStmt = expr
  ";"` forces the `;`. The name is permitted (`fn foo() {}` parses as a *named* fn literal —
  `parse_fn`'s Ident arm mod.rs:238-242) but the parser binds nothing: the result is `Stmt::Expr
  (Expr::Fn(proto))`, an expression statement whose value is a closure; whether `foo` becomes callable
  is entirely a later pass's decision (out of scope here — not verifiable from these two files).
  Top-level differs at `parse_declaration` mod.rs:165-168 (`Kw(Fn) | Kw(Async)` arm →
  `parse_fn(false, vis)`, mod.rs:226-276, which never expects `;` after the body).
- **Related gap:** named fn literals in expression position (`let f = fn go() { };`) are accepted,
  though the frozen grammar's `fnLit = [ "async" ] "fn" "(" [ params ] ")" block` is anonymous-only —
  undocumented extension, same mechanism.
- **Block-vs-expression ambiguity:** none — `fn` cannot start a block; a statement-initial `fn(x){…}`
  is unambiguously the fn-literal exprStmt. `if`/`while`/`for` bodies must be blocks, so a bare
  `fn(x){…}` there is a clean "expected '{'" error.
- **Fix direction:** Either add `fnDecl` to the block-level `statement` production (and a parser arm
  that consumes no `;` and produces a declaration node), or document the exprStmt rule and the
  named-literal extension in GRAMMAR.md.

### P8 [minor] C-style `for let` header is more permissive than the grammar
- **Where:** dispatch mod.rs:607-620 (`peek_kind(1) == Kw(In)` selects ForIn — correct for both
  forms); header mod.rs:622-635 via `parse_let_inner(false, false)` mod.rs:522-549.
- **What breaks:** Grammar: `"for" "let" ident "=" expr ";" expr ";" expr block`. The code (a) makes
  the init value optional — `for let i; i < 3; i = i + 1 { }` parses; (b) accepts a type annotation —
  `for let i: int = 0; …`. Both are undocumented extensions. The post expression is required
  (grammar-conformant); an omitted cond/post gives a generic `unexpected ';'` from `parse_primary`
  (mod.rs:1003) rather than a for-specific hint.
- **Repro:** `for let i; i < 3; i = i + 1 { }` — parses without error.
- **Fix direction:** Require `= expr` per the grammar or extend the EBNF; add a for-specific message
  for empty cond/post.

### P9 [minor] Assignment accepted as `if`/`while` condition (grammar-conformant footgun)
- **Where:** `parse_if` mod.rs:574-575 and `parse_while` mod.rs:601-602 call `parse_expr`, which tops
  out at `parse_assign` (mod.rs:682-684).
- **What breaks:** `if x = 5 { }`, `while x = next() { }`, even `if x += 1 { }` parse as conditions
  containing an assignment. The frozen grammar (`ifStmt = "if" expr block`, `expr = assignment`)
  permits it, so this is not a code/doc conflict — but it is the classic `=`-for-`==` trap with no
  parser-side guard or warning.
- **Repro:** `if x = 5 { }` — parses (silently assigns if sema allows).
- **Fix direction:** Restrict conditions to non-assignment expressions in the parser, or emit a
  warning; document the choice.

### P10 [minor] No error recovery: the first bad token aborts the whole file
- **Where:** every `parse_*` returns `SpidResult` and all callers propagate with `?`; the top-level
  loop mod.rs:120-138 has no sync/resync logic; `Diagnostic` is returned singly (one error per file).
- **What breaks:** One stray token (e.g. a missing `;`) suppresses every later diagnostic in the file;
  no panic, just fail-fast by design. Also relevant to edit-loop UX: errors always restart parsing
  from scratch on the next attempt.
- **Repro:** `let a = 1 let b = 2;` → only the first error is reported.
- **Fix direction:** If recovery is wanted: sync on `;` / `}` / statement-starting keywords inside
  `parse_block` and collect a `Vec<Diagnostic>`; otherwise document fail-fast as intentional.

### P11 [minor] Visibility and class-body extensions beyond the frozen grammar
- **Where:** `parse_declaration` mod.rs:153-221 consumes `public`/`private` before class (171),
  interface (172), and top-level let/const (173-199, re-wrapping the node with the vis) — GRAMMAR
  grants `[visibility]` only to `fnDecl`. Class body: constructor shorthand `new(params) { … }`
  mod.rs:360-377 and `fn new` as a method name mod.rs:243-247 — `member` in the grammar has neither.
- **What breaks:** Nothing crashes; programs the frozen grammar cannot describe are accepted. A
  reviewer implementing from GRAMMAR.md alone would reject `public class` / `new(...) { }`.
- **Repro:** `public class A { new() { } }` — parses.
- **Fix direction:** Update GRAMMAR.md (`declaration = [visibility] …`, add `ctorDecl` to `member`),
  or restrict the parser.

### P12 [minor] Generic types support exactly one type argument
- **Where:** `parse_type` mod.rs:300-312: `ident [ "<" type ">" ]` — after the inner type it
  `expect(Gt)`, so a `,` is an error.
- **What breaks:** `let m: map<string, int> = …;` → `expected '>', found ','`. This matches the frozen
  `type = ident [ "<" type ">" ]`, but it means two-parameter generic types cannot be annotated
  anywhere; GRAMMAR's own notes reference `hash_table` as an index-assignment target without any
  annotatable spelling. (Docs contain no `map<`/`hash_table<` example — verified by grep — so impact
  is unproven but structural.) Nested one-arg generics work: `list<list<int>>` lexes as two `Gt`
  tokens (no `>>` token exists) and parses.
- **Repro:** `let m: map<string, int> = null;` → parse error.
- **Fix direction:** Extend grammar+parser to `type = ident [ "<" type { "," type } ">" ]` if 2-arg
  types are intended.

### P13 [minor] Comparison chains accepted, left-associative (grammar-conformant; sema footgun)
- **Where:** `parse_cmp` loop mod.rs:755-778.
- **What breaks:** `a < b < c` parses as `Lt(Lt(a,b), c)` and `a == b == c` as `Eq(Eq(a,b), c)` — the
  grammar's `{ cmpOp addExpr }` repetition implies exactly this, so no doc conflict, but the inner
  result is a bool being compared again; there is no non-associativity restriction or warning.
  Verified-OK next to it: `-`, `/`, `%` are left-associative (correct math), and unary binds tighter
  than `%` (rows 2–3).
- **Repro:** `let x = 1 < 2 < 3;` — parses.
- **Fix direction:** Reject mixed comparison chains in the parser (`a op b op c` → "comparisons cannot
  be chained") or leave to sema; document.

### Verified-OK (checked, no finding)
- **Dangling else / elif chains:** blocks are consumed atomically, so `else` always binds to the
  nearest unfinished `if`; any number of `elif`s; `else if` (block-less) is a clean "expected '{'"
  error; an `elif` after `else` errors at top level on `elif` as an expression start (mod.rs:571-597).
  Matches the GRAMMAR note.
- **Call chains on literals:** `"abc".upper()` (Str primary mod.rs:945-948 → postfix Dot/Method
  mod.rs:886-907), `(1).foo()` (paren primary → postfix), `1.foo()` (lexer splits `1` `.` `foo`) all
  parse; `[1,2].push(3)` likewise.
- **EOF robustness:** `peek_kind` clamps at the trailing Eof (mod.rs:33-37); block/class/interface
  loops diagnose "unterminated …" on EOF (mod.rs:441-447, 332-335, 411-414) instead of panicking; no
  indexing-panic path found (`kind()` stays in range because no helper advances on a failed match).
- **Import ordering:** `program = { import } { declaration }` is enforced with a clear error
  (mod.rs:122-131).
- **Semicolon table:** `;` required after import/let/const/return/break/continue/throw/spawn/exprStmt
  (hence local fn, see P7)/class field/interface method; never after if/elif/else, while, for, try,
  block, top-level fnDecl, class, interface — matches GRAMMAR except where noted (P5, P7).
- **No trailing commas** in params/args/list literals — grammar-conformant (`param { "," param }`).
- **`self` handling:** accepted as param name and as expression (`Expr::Ident("self")`,
  mod.rs:957-960; `expect_ident` accepts `SelfKw` mod.rs:99-107). Nits beyond grammar: typed
  `self: T` params and `x.self` field access parse.
- **Dead code:** `parse_statement`'s `#[allow(unreachable_patterns)] _ if false => unreachable!()`
  (mod.rs:507-508) is harmless cruft.
- **`join()` span math:** composite spans on one line are computed correctly; multi-line composites
  deliberately keep the start token's width (mod.rs:1027-1040) — diagnostic width only, consistent
  with lexer finding L4.

---

## Totals
Lexer: 1 major (L1), 9 minor (L2–L10). Parser: 2 major (P1, P2), 11 minor (P3–P13).
No critical findings: nothing accepts wrong code silently; both majors are crashes (L1 debug panic /
garbage span; P1 stack-overflow abort) or rejection of doc-valid programs (P2).
