# 02 — Static inspection: Spid3r semantic checker

- Scope: `src/semantic/mod.rs` (~1959 lines) and `src/semantic/ty.rs` (79 lines), working tree as of 2026-10-20.
- Method: static reading only. `cargo`/`rustc`/execution forbidden by task, so nothing here was run; every claim is a code trace with file:line. Sibling dirs (parser, interpreter, vm, stdlib, tests) were grepped **only** to confirm API contracts, and those citations are marked as such.
- Doc claims re-verified, not trusted: README.md:106 ("static type, arity, and privacy check"), README.md:110 ("Every executable program defines `fn main()`"), README.md:371-372 ("checker catches unknown names, private-member access, arity errors, and type-annotation mismatches before any code runs"), README.md:548 ("Name resolution, arity, privacy, `main` validation, and gradual type checker"). Prior audit note `audit_notes/fix_semantic_priv.md` treated as claims; its F6/F12/F5 fix claims were re-checked in code (verdict below).

## Verdict summary

The checker is real and mostly does what the README says for **locally declared** classes and **directly validated** annotation positions: name resolution, duplicate/import-collision detection, arity for direct and signature-carrying calls, privacy (including field writes), and return/break/continue/self/await context rules all check out. But there are four concrete holes of Medium severity — a bypass path through index-assignment targets, existence-only `main` validation, signature-blind function assignability, and silently-degraded annotation typos — plus two false-positive sources (`option`/`result` annotations, `abs` return type). One suspected panic (`arity - 1` underflow) turned out to be **latent only** after tracing parser reachability. Confidence per finding below; overall verdict: **high confidence**, all findings traced end-to-end, none executed.

---

## Findings (ranked)

### F1 — Index-assignment targets never check the index expression (name-resolution bypass)

- **Severity: Medium · Confidence: High (code path fully traced; reachability confirmed via interpreter contract)**
- Evidence: `src/semantic/mod.rs:1694`
  ```rust
  Expr::Index { object, .. } => self.check_expr(object)?,
  ```
  The `Assign` arm pattern discards `index`; after the match only the assigned `value` is re-checked (`self.check_expr(value)`, end of the `Expr::Assign` arm, mod.rs:1744 region). Compare the **read** path, which checks both sides: mod.rs:1866-1870 (`Expr::Index { object, index, .. }` → `check_expr` on each).
- Mechanism: the unknown-name check lives in `check_expr` (mod.rs:1615-1623). Any expression placed inside an assignment index — `arr[ghost] = 1;`, `m[f(1, 2, 3)] = 2;` — therefore escapes **all** semantic checking: unknown names, call arity, const-assignment, everything. At runtime the interpreter *does* evaluate that index (`src/interpreter/mod.rs:782-785`, Assign over `Expr::Index` target evaluates `index`), so the program fails at run time with a runtime error that the front end claimed to catch.
- User impact: `spid3r check` exits clean on code that is guaranteed to fail at run time; typos inside index targets are invisible to the checker. Directly contradicts README.md:371 ("checker catches unknown names … before any code runs").
- Fix direction: in the `Assign` arm, `Expr::Index { object, index, .. } => { self.check_expr(object)?; self.check_expr(index)?; }`.

### F2 — `main` validation is existence-only; signature is never checked

- **Severity: Medium · Confidence: High**
- Evidence: `src/semantic/mod.rs:559-564`
  ```rust
  if self.is_main && !self.funcs.contains_key("main") {
      return Err(Diagnostic::new(Phase::Semantic,
          "a program run by `spid3r run` needs fn main()"));
  }
  ```
  That is the entire `main` check. `FuncInfo` records `main`'s arity and params (mod.rs:314-322) but nothing consumes them for `main`.
- Runtime contract (grep-level confirmation, out-of-scope dir): `src/interpreter/mod.rs:224-242` resolves `main` from globals and calls it with `Vec::new()` — zero arguments, unconditionally; `src/vm/mod.rs:278-280` does the same for the VM path. A non-function `main` and a missing `main` are caught at runtime (interpreter/mod.rs:226-239), but an over-parameterized `main` is not caught anywhere before call time.
- Consequence: `fn main(x) { }` **passes** `spid3r check` and `spid3r run` dies with a runtime arity error on every execution — a statically-known, statically-fixable defect. README.md:548 claims "`main` validation"; README.md:371 claims arity errors are caught "before any code runs". Both overclaim for this shape. (Related, lesser: a non-`int` return from `main` silently becomes exit code 0, interpreter/mod.rs:251-254.)
- Fix direction: in pass 1, when `is_main`, require `FuncInfo.arity == 0` (optionally: ret absent or `int`) with a clear semantic diagnostic.

### F3 — Function-to-function assignability ignores signatures; function bindings are re-assignable

- **Severity: Medium · Confidence: High**
- Evidence: `src/semantic/mod.rs:1519`
  ```rust
  (Ty::Fn(_), Ty::Fn(_)) => true,
  ```
  `Ty::Fn` carries a full `FnSig { arity, params, ret }` (src/semantic/ty.rs:64-79) precisely so calls can be validated — but `assignable` never compares them. Meanwhile top-level fn names are declared with `is_const: false` (mod.rs:327-344), and the `Assign`-Ident arm only rejects consts (mod.rs:1663-1667).
- Mechanism (both directions unsound):
  - *False accept*: `fn f(x: int) -> int { return x; }  fn h() { }  let g = f;  g = h;  g(1);` — `g = h` passes (`Fn ≈ Fn`), and `g(1)` is then checked against **f's stale signature** (VarInfo.ty is not updated by assignment), so the checker approves a call that invokes arity-0 `h` with 1 argument → guaranteed runtime failure. The same works re-binding a top-level `fn` name directly (`f = h;`).
  - *False positive*: after the same rebinding, the now-correct `g()` is rejected with "expects 1 argument(s)" (stale sig, mod.rs:1777-1795).
- User impact: annotated function values give a false sense of safety; diagnostics after rebinding point at the wrong signature.
- Fix direction: compare `FnSig` (arity + annotated param types + ret) in `assignable`, and/or make function-declared bindings const.

### F4 — Annotation typos silently degrade to `Any` in most positions

- **Severity: Medium · Confidence: High**
- Evidence: `check_type_name` is invoked at exactly four sites — mod.rs:808 (top-level fn **return**), mod.rs:912 (`let`), mod.rs:981 (`const`), mod.rs:1086 (for-init `let`). It is **not** invoked for:
  - fn parameters — mod.rs:785-805 (param `ty` goes straight through `resolve_type`);
  - constructor parameters — mod.rs:842-856; method parameters — mod.rs:872-886;
  - method return types — mod.rs:891-892;
  - class field types — mod.rs:442-447 (registry inserts `(f.vis, f.ty)` unvalidated);
  - lambda parameters/returns — mod.rs:1634-1651.
- Mechanism: `resolve_type`'s fallback maps every unknown name to `Ty::Any` (mod.rs:1236-1244, `_ => Ty::Any` with the doc comment at mod.rs:1210-1213 assuming "check_type_name has already rejected truly unknown names" — an assumption that does not hold for the positions above). So `fn f(x: strng) { }` (typo) compiles cleanly with argument checking for `f` silently disabled; `class C { y: Instani }` disables field type checks for `y`. The identical typo in `let x: strng = 1;` errors "unknown type 'strng'" (mod.rs:1596-1603) — the inconsistency proves the omission is unintended.
- User impact: gradual typing is being used as a typo sink; README.md:371's "type-annotation mismatches" claim silently does not apply to these positions.
- Fix direction: call `check_type_name` for every `TypeExpr` (or move validation into `resolve_type` and make the fallback unreachable).

### F5 — `option`/`result` annotations can never be satisfied by their constructors

- **Severity: Low-Medium · Confidence: High**
- Evidence: `resolve_type` maps `option`/`result` and `some`/`none`/`ok`/`err` to **distinct** `Coll` names (mod.rs:1225-1231); `some`/`none`/`ok`/`err` are builtin constructors (BUILTIN_CONSTRUCTORS, mod.rs:211-214), so `type_of(some(5))` = `Coll("some", …)` (mod.rs:1424-1440 constructor branch); `assignable` compares `Coll` names only (mod.rs:1518).
- Consequence: `let x: option<int> = some(5);` is rejected — "type mismatch in 'x': expected option, got some" — and likewise `result<int>` vs `ok(1)`. The annotated-option surface is unusable as specified. Cross-check: `tests/` contains zero occurrences of `option<`/`result<` (grep), so nothing pins or protects this behavior.
- Fix direction: either alias `some`/`ok`… to `option`/`result` at `resolve_type`/`type_of`, or normalize Coll names in `assignable`.

### F6 — `builtin_return_type("abs")` is wrong for float input (false rejection)

- **Severity: Low-Medium · Confidence: High**
- Evidence: checker hardcodes `abs → Ty::Int` (mod.rs:344). Runtime is polymorphic: `src/stdlib/mod.rs:606-610` — `Value::Int(i) => Value::Int(i.wrapping_abs())`, `Value::Float(f) => Value::Float(f.abs())`.
- Consequence: `let y: float = abs(-2.5);` is rejected ("expected float, got int") although the program is correct and returns a Float at runtime. Valid annotated code is blocked.
- Cross-check: `pow → Float` (mod.rs:347) **matches** runtime (stdlib/mod.rs:628-630 always returns `Value::Float`); the inaccuracy is specific to `abs` among the natives I sampled.
- Fix direction: return `Any` for polymorphic natives (or a union concept); at minimum move `abs` out of the Int row.

### F7 — Collection element types are parsed, tracked, then erased

- **Severity: Low (documented gradual behavior) · Confidence: High**
- Evidence: ty.rs:26-28 — element type "is tracked best-effort for diagnostics but is *erased* when deciding assignability"; `assignable`: `(Ty::Coll(a, _), Ty::Coll(b, _)) => a == b` (mod.rs:1518).
- Consequence: `let xs: list<int> = ["a", "b"];` passes the checker. Combined with F4 this means element annotations are purely decorative today. Listed because README.md:106's "static type … check" reads stronger than what is implemented; the ty.rs comment keeps this honest as a design gap, not a bug.

### F8 — Latent `usize` underflow in the method-arity diagnostic (currently unreachable)

- **Severity: Low (latent) · Confidence: High (after reachability trace)**
- Evidence: mod.rs:1826-1839 — when `args.len() + 1 != arity`, the message evaluates `arity - 1` (mod.rs:1836). `MethodInfo::arity()` is `params.len()` (mod.rs:24-28) and constructor params are stored **without** `self` (registry mod.rs:452-466, ctor detection mod.rs:466-474), so a zero-param constructor yields `arity == 0`, and `0usize - 1` would panic in a debug build / print `usize::MAX` in release.
- Why it is *latent*: the only zero-arity entry in the registry is a `new` constructor (ordinary methods must take `self`, enforced at mod.rs:466-474 → arity ≥ 1), and the `Expr::Method` arm is the only caller that subtracts. Reaching it would require source like `p.new()` or `self.new()`, but the parser's postfix `.` handler reads the member name with `expect_ident` (src/parser/mod.rs:976-979), which accepts only `Ident` and `SelfKw` (parser/mod.rs:83-100) — `.new` is a **parse error**. (A different helper does accept `Keyword::New` as a name — parser/mod.rs:250-260 — but it is not the one used after `.`.)
- Residual risk: any future parser relaxations ("allow keywords as member names") or a new caller of `method_info` on ctor names turns this into a checker panic, violating the project's own invariant "user code never panics the host" (audit_notes/fix_semantic_priv.md). Related oddity while tracing: with a 1-param ctor, `p.new()` would pass the arity equation (`args.len() + 1 == arity`); moot while `.new` is unparseable.
- Fix direction (cheap): `arity.saturating_sub(1)` or special-case `name == "new"` in the `Expr::Method` arm.

### F9 (minor) — dead `null` arm in `resolve_type`

- `resolve_type` maps `"null" => Ty::Null` (mod.rs:1219-1220), but `check_type_name`'s TYPES table (mod.rs:1559-1596) has no `"null"`, so `let x: null = …;` always fails with "unknown type 'null'". The resolve arm is unreachable for annotations. Harmless inconsistency; align the two tables.

### F10 (minor, gray zone) — calling a class name like a function passes

- `Point("x")`: not in `funcs`, no `Fn` sig, but `known()` returns true via the class registry (mod.rs:594-607), so the call is accepted unchecked (mod.rs:1749-1773 / 1777-1795 fallthrough). Constructors actually require `new` (mod.rs:1898-1926). Statically-known misuse left to runtime; noted for completeness, not ranked.

---

## Acceptance items (explicit verdicts)

**(b) Is `main` signature validation actually enforced?** No. Enforced: `main` must exist when `is_main` (mod.rs:559-564) and, at runtime, must be a function. Not enforced: arity/params/return — `fn main(x)` passes the checker and fails at call time (interpreter/mod.rs:241 calls with 0 args). See F2.

**(c) Is privacy enforced at check time?** Yes for locally declared classes, no for imported ones.
- Enforced: method calls (mod.rs:1808-1824, `receiver_class` + `method_info`), field reads (mod.rs:1873-1893), **field writes** (mod.rs:1697-1723 — the audit note claimed this was previously missing; the fix is present, including the comment at 1697-1699), interface conformance requires public methods (mod.rs:506-516), inheritance-aware lookup with cycle guards (`field_vis` mod.rs:596-608, `method_sig` mod.rs:618-633), `private` is the parse-level default (src/parser/mod.rs:361-364, comment "private is the default"). In-class access is keyed on `current_class` (class-level, not instance-level, semantics — consistent).
- Hole: `receiver_class` deliberately returns `None` for imported/builtin receivers (doc mod.rs:1246-1250, code 1251-1264); import bindings carry neither `class` nor `ty` (mod.rs:692-707), so `p.secret` on an instance of an imported class gets **no** privacy or existence check at check time. Cross-module privacy is therefore deferred to runtime; README's blanket "enforced at check time" (README.md:106/371/548) overclaims for that path.

**(d) Type-checker bypass paths?** Confirmed: F1 (index-assign target), F3 (fn rebinding), F4 (typo annotations → `Any`), F7 (element erasure). **Not** bypass paths, verified: lambda/closure annotations are honored — lambda params/ret resolved (mod.rs:1634-1651), signatures propagate through `let`/nested fn via `fn_sig_type`/`fn_value_type` (mod.rs:1267-1297) and calls are validated through `check_sig_args` (mod.rs:1300-1330, used at 1777-1795); collection-literal element inference is conservative (`Any` on first mismatch, mod.rs:1377-1390).

**Re-verification of audit_notes/fix_semantic_priv.md:** its code claims check out — `receiver_class` exists and is used in Method/Field/Assign-Field arms; F12 import/local collision diagnostics exist in Import/Let/Const/Fn arms (mod.rs:673-742, 944-955, 999-1010, 760-773); F5 sig propagation exists (ty.rs:57-79, mod.rs:1267-1330). Its "tests 19/19 green / 340 core green" claims were **not** re-run (execution forbidden) — status unverifiable here.

---

## What is SOUND

- **Name resolution & scoping**: two-pass predeclaration of top-level fns (mod.rs:289-346) makes mutual recursion work; duplicate detection covers fn/class/interface/let/const (mod.rs:289-346); same-scope redeclaration rejected (`declare`, mod.rs:570-583); import-collision diagnostics in both directions (F12 above).
- **Arity**: direct calls (mod.rs:1750-1773), signature-carrying callees incl. nested fns/`let g = f`/lambdas (mod.rs:1777-1795, 1300-1330), methods with `self` accounting (mod.rs:1826-1841), constructors (mod.rs:1898-1926), interface conformance arity (mod.rs:517-530).
- **Context rules**: `return` outside fn (mod.rs:1126-1129), `break`/`continue` outside loop (mod.rs:1166-1180), `self` outside method (mod.rs:1610-1614), `await` outside async (mod.rs:1942-1947), `spawn` requires a fn literal (mod.rs:1181-1187, 1933-1940), const assignment rejected (mod.rs:1663-1667).
- **Panic hygiene**: the only `unwrap`s are mod.rs:520 (class was just inserted into the registry at 466-473), and `scopes.last_mut()` at 574/698/732 (scope stack starts non-empty, `vec![]` init at 254, push/pop balanced with decrements on error paths at 810-816, 891-897, 1649-1655) — no reachable panic found besides the latent F8.
- **Grammar coverage**: `for` init is always a `let` by grammar (src/parser/mod.rs:669-671), so the checker's `if let Stmt::Let` on ForC init (mod.rs:1079-1113) is total — no gap there.
- **ty.rs**: no panics, no casts; `Ty`/`FnSig` are plain data; the only weak spot is the documented erasure (F7).

## Not done / 待证 (out of time or out of scope)

- Nothing was executed (task forbids cargo/rustc): F8's debug-panic vs release-wrap behavior is stated from Rust semantics, not observed. A one-off repro would settle it if execution is ever allowed.
- Whether the VM path duplicates any checker gaps differently (vm/mod.rs:278-280 was only sampled for the `main` contract).
- `tests/fix_semantic_priv.rs` and `tests/gradual_typing_tests.rs` not audited for assertions that *pin* the F5/F7 behavior as intended.
- Cross-module privacy at runtime (interpreter side) — out of boundary; only the checker-side gap is claimed here.

## Suggested next steps (if continued)

1. F1 + F4 are the smallest diffs with the largest silent-failure payoff (one-arm fix; four `check_type_name` insertions).
2. F2: add `main` arity/ret validation in pass 1 where `FuncInfo` is already at hand.
3. F3: decide the contract — either compare `FnSig` in `assignable` or make fn bindings const; add a regression test for the rebinding sequence.
4. F5/F6 need a product decision (option/result naming; polymorphic native returns) more than a code fix.
5. F8: one-line `saturating_sub` now, so the invariant "user code never panics the host" survives future parser changes.
