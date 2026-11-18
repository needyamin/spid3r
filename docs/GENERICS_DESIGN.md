# R10.1 — Generics, Monomorphization & Unboxing Design (`DP-1` Analysis)

> **Status:** Approved (2026-11-18, owner sign-off on `DP-1` + `R10.1`).
> **Scope:** Contract for `R10.2` (Parser + Checker Instantiation) and `R10.3` (AOT Monomorphizer + Unboxed Containers).

---

## 1. Decision Point `DP-1` Analysis (`Value` v2 & RED LINE Amendment)

### Current State
- `src/value/mod.rs` defines `pub enum Value` (heap-boxed via `Box<Value>` across `src/native/rt.rs` FFI trampolines, passed as `*mut Value`).
- Homogeneous collections (`List(Ref<ListData>)`, `Map(Ref<MapData>)`, etc.) store `Vec<Value>` / `HashMap<String, Value>` behind `Arc<Mutex<...>>`.
- `src/native/ir.rs` already performs local scalar unboxing (`top_ints`, constant folding, `small_int_ptr` cache), but every container element read/write, generic function call, and cross-trampoline boundary still boxes into `*mut Value`.
- As documented in `docs/PERF_TARGET_RETIREMENT.md` and `docs/html/benchmark.html`, this boxing + `Arc<Mutex<...>>` overhead accounts for the **8–18×** container gap on `json` / `map` vs Node/Rust.

### `DP-1` Proposal
Amend the RED LINE to permit a **Value v2** representation alongside AOT monomorphization:
1. **Engine / VM / Tree (`Value` v2):** Compact 64-bit tagged value (`NaN`-boxed or low-tag `u64` inline scalars for `Int` (small/inline), `Float`, `Bool`, `Null`, and tagged pointers for heap objects) — eliminating `Box<Value>` heap allocations for scalar values and reducing `Value` size from multi-word enum to 8 bytes.
2. **AOT Unboxed Containers (`R10.3`):** Homogeneous typed container declarations (`list<int>`, `list<float>`, `map<string, int>`, etc.) lower in AOT (`src/native/ir.rs`) to specialized unboxed backing buffers (`Vec<i64>`, `Vec<f64>`, typed hash map) without per-element `Value` boxing or `Mutex` locking on single-threaded/unshared paths, while preserving identical Spid3r language semantics (`docs/ENGINE_CONTRACT.md`).
3. **Safety & ABI Guardrails (`SEC-A`..`D`):**
   - All `unsafe` pointer/tag operations stay strictly confined to `src/native/rt.rs` (and `src/value/` if inline-tagged `Value` v2 uses `NonNull`/`usize` bitpacking, each with a written `// SAFETY:` invariant).
   - `docs/selfhost/RUNTIME_ABI.md` ABI version bumps cleanly if trampoline signatures change; `selfhost/` continues to use the existing `spid3r_*` ABI until explicitly migrated.

---

## 2. Erased-Syntax Backward Compatibility

Existing Spid3r programs (guarded by `tests/constructs.rs::GENERICS`) use **erased** generic syntax without explicit type arguments at call sites or on fields:
```matrix
class Box<T> {
    public let value;
    fn new(value) { self.value = value; }
    public fn get(self) { return self.value; }
}
fn first<T>(xs) { return xs[0]; }
fn identity<T>(x) { return x; }
fn head<T>(xs) -> T { return xs[0]; }

let b = new Box(7);
assert_eq(first([3, 4, 5]), 3);
assert_eq(identity("x"), "x");
```
**Contract:**
- Generic type arguments at call/construction sites (`identity<int>(7)`, `new Box<int>(7)`) remain **optional**.
- When omitted, the checker infers concrete type arguments from call arguments where possible, or falls back to `Any` (dynamic `Value` representation).
- Unconstrained `<T>` with untyped parameters (`fn first<T>(xs)`) continues to type-check and execute identically across Tree, VM, WASM, and AOT.

---

## 3. Parser & AST Changes (`R10.2`)

1. **`src/ast/mod.rs`:**
   - Replace `TypeExpr.arg: Option<Box<TypeExpr>>` with `args: Vec<TypeExpr>` so multi-parameter generic types (`map<string, int>`, `result<int, string>`, `Pair<int, bool>`) are represented directly.
   - Add `pub struct TypeParam { pub name: String, pub bound: Option<String>, pub span: Span }` (replacing `Vec<String>` on `FnProto.type_params` and `ClassDecl.type_params`, or keeping a helper `.names()` for minimal diff).
2. **`src/parser/mod.rs`:**
   - Update `parse_type_params()` (currently skips tokens inside `<...>`) to parse comma-separated `TypeParam` entries with optional constraint bounds (`<T>`, `<T: Comparable>`, `<K, V>`).
   - Update `parse_single_type()` to parse `<T1, T2, ...>` into `TypeExpr.args`.
   - Support optional explicit type arguments on function calls (`foo<int>(x)`) and constructor calls (`new Box<int>(x)`), disambiguating `<` from less-than using lookahead (`<` followed by type list + `>(`).

---

## 4. Checker Instantiation & Constraint Model (`R10.2`)

In `src/semantic/mod.rs`:
1. **Type-parameter scope:** Bind declared `TypeParam`s with their optional constraint (`Comparable`, `Numeric`, `Printable`, or interface name) during function/class checking.
2. **Constraint checking:** At instantiation sites (explicit `<int>` or inferred from arguments), verify each concrete type satisfies the declared bound; emit a `Phase::Semantic` diagnostic with span on mismatch or wrong type-arg arity.
3. **Instantiation recording:** Record a deduplicated `InstantiationSet` on the checked program metadata:
   - `MonoFnKey { name: String, type_args: Vec<Ty> }`
   - `MonoClassKey { name: String, type_args: Vec<Ty> }`
   - Typed container annotations (`list<int>`, `list<float>`, `map<string, int>`, etc.) propagated to local variable / parameter / return type tables.
4. **Tree & VM parity:** Tree (`src/interpreter/mod.rs`) and VM (`src/vm/`) execute generic functions and classes polymorphically on `Value` with **zero** behavioral divergence from AOT monomorphized code (`docs/ENGINE_CONTRACT.md`).

---

## 5. AOT Monomorphization & Unboxed Containers Plan (`R10.3`)

In `src/native/ir.rs` and `src/native/rt.rs`:
1. **Monomorphized LLVM IR Functions:**
   - For each concrete `MonoFnKey` where all type args are scalar/known (`int`, `float`, `bool`, `string`, typed container), emit a specialized LLVM function `spid3r_mono_<fn>__<mangled>` taking/returning unboxed LLVM types (`i64`, `double`, `i1`, or typed container pointer) where safe.
   - Bounded instantiation depth (e.g., max 64 specializations per generic function) with automatic fallback to the generic `*mut Value` entry if exceeded or if `Any`/heterogeneous types appear.
2. **Unboxed Containers (`list<int>`, `list<float>`, `map<string, int>`):**
   - Introduce `rt.rs` typed-buffer handles (or specialized `ListI64` / `ListF64` / `MapStrI64` trampolines) used **only** when the checker proves the variable/parameter never escapes to untyped `any` / dynamic `Value` contexts (or auto-boxes on escape).
   - Indexing `xs[i]`, `.push(v)`, `.len()`, and `for x in xs` lower directly to unboxed loads/stores without `Box::into_raw` or `Arc<Mutex<...>>` per element.
3. **Targets & Gates (`R10.3`):**
   - HLC corpus codegen parity `<= 1.25x`.
   - `tools/phase0_bench.py` Tier-B improvement on `json` and `map` (before/after ledger rows).
   - `tools/selfhost_native_parity.sh` unchanged (selfhost sources do not use generics).

---

## 6. Corpus & Parity Impact List

| Surface | Impact | Action |
|---|---|---|
| `selfhost/corpus/` (134 programs) | **0** existing programs use `<T>` generics | Frozen corpus parity unchanged; add new generic instantiation corpus cases under SH-4 ledger entry in `R10.2`. |
| `selfhost/*.matrix` (17 modules) | **0** modules use generics | `selfhost_lex_parity.sh`, `selfhost_parse_parity.sh`, `selfhost_semantic_parity.sh`, `selfhost_native_parity.sh`, `selfhost_bootstrap_parity.sh` remain green without requiring selfhost syntax changes. |
| `tests/constructs.rs` (`GENERICS`) | Uses erased `Box<T>`, `first<T>`, `identity<T>`, `head<T>` | Must continue to pass identically in Tree, VM, and AOT. |
| `tests/engine_matrix.rs` | Needs new generic instantiation & constraint rows | Add tree == vm == native parity rows in `R10.2` and `R10.3`. |
| `tools/runtime_artifact.sh` / `RUNTIME_ABI.md` | New `spid3r_list_i64_*` / `spid3r_map_str_i64_*` trampolines in `R10.3` | Update trampoline counts & ABI docs when `R10.3` lands. |
