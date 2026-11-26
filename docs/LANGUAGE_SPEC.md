# Spid3r Formal Language Specification

> **Status:** Production specification for Spid3r v1.2 (stage-0 Rust compiler v1.1.1 / LLVM 23.1.0 AOT backend + self-hosted `spid3r-self`).
> **Companion Specifications:** [GRAMMAR.md](GRAMMAR.md), [GENERICS_DESIGN.md](GENERICS_DESIGN.md), [GC.md](GC.md), [CONCURRENCY.md](CONCURRENCY.md), [ENGINE_CONTRACT.md](ENGINE_CONTRACT.md), [NATIVE.md](NATIVE.md), [BYTECODE.md](BYTECODE.md), [FFI.md](FFI.md), [SECURITY.md](SECURITY.md), [STANDARD_LIBRARY.md](STANDARD_LIBRARY.md).

---

## 1. Language Model & Scope

Spid3r is a general-purpose, gradually and statically checkable programming language (`.matrix` / `.mt` source files) with four **Spid3r Engine** execution backends (`tree` reference interpreter and `vm` bytecode VM under `spvm_rt` / `spvm`, **YamLLVM** LLVM 23.1 AOT `native` via `yamc` / `spid3r build`, and `wasm32`) plus a self-hosted native compiler pipeline (`selfhost/`, `spid3r-self`), bytecode compiler (`yamcvm` → `.matrixc`), package manager (`spkg` / `spider.toml`), and **YamGC** memory reclamation.

Core surface inventory (verified against the repository):
- **26** embedded `.matrix` standard library modules (`stdlib/`)
- **254** built-in native standard library functions (`src/stdlib/`) split into core primitives (`core_defs`), freestanding subset (`freestanding_defs`), and **10** opt-in subsystem groups (`json`, `fs`, `net`, `db`, `media`, `tz`, `unicode`, `crypto`, `data`, `info`)
- **45** `spid3r.ini` configuration keys across **12** sections (`src/config/schema.rs`)
- **3** optional Cargo features (`gui`, `native`, `tls`); core builds cleanly with zero external crate dependencies (`--no-default-features`)
- **136** frozen parity corpus programs (`selfhost/corpus/`, pinned by `MANIFEST.sha256`)

Syntax and EBNF grammar are formally frozen in [GRAMMAR.md](GRAMMAR.md).

---

## 2. Type System, Gradual Checking & Nullability Narrowing

Spid3r provides **optional gradual static type checking** enforced at check/compile time (`src/semantic/mod.rs`, `src/semantic/ty.rs`):

### 2.1 Type Universe (`Ty`)
- **Primitives:** `int` (signed 64-bit integer), `float` (IEEE 754 `f64`), `bool`, `string` (immutable UTF-8), `null`.
- **Dynamic escape hatch:** `any` (also the implicit static type of unannotated bindings and parameters).
- **Parameterized Collections & Algebraic Wrappers (`Ty::Coll`):** `list<T>`, `vec<T>`, `map<K, V>` / collection element annotations checked covariantly when both sides carry concrete annotations. Built-in tagged wrappers include `option` (`some(v)` / `none()`) and `result` (`ok(v)` / `err(e)`).
- **Nominal & Structural Types:** `class`, `interface`, `enum` variants.
- **Nullable & Union Types:** `T?` (`Ty::Nullable(T)`) accepts `T` or `null`; union types `A | B` (`Ty::Union`) accept any constituent member type.
- **First-Class Function Types:** `fn(P1, P2, ...) -> R` (`Ty::Fn(Option<Arc<FnSig>>)`), preserving parameter arity, trailing default minimums (`min_arity`), and return annotations across lambdas and variable bindings.

### 2.2 Flow-Sensitive Nullability Narrowing (`R6.3`)
The semantic checker maintains a lexical stack of flow-narrowing overlays (`narrowed: Vec<HashMap<String, Ty>>`) and refines nullable/union/option/result types (`non_null_ty`) inside guarded control flow (`extract_null_guards`):
- **Positive/Negative branch refinement:** `if x != null { ... }` narrows `x` from `T?` / `T | null` to `T` in the `then` branch and `null` in `else`; symmetrically for `x == null`, `is_null(x)`, logical `not`, `and`, and `or`.
- **Early-exit / diverging guard refinement:** When an `if cond { return ...; }` or `throw` guard always diverges (`block_always_diverges`), the negated narrowing condition remains active for subsequent statements in the enclosing block.
- **Assignment invalidation:** Reassigning a narrowed variable updates or clears active narrowing overlays conservatively so stale assumptions never survive mutation.

### 2.3 Generics & AOT Monomorphization (`DP-1`, `R10`)
Generics follow the specification in [GENERICS_DESIGN.md](GENERICS_DESIGN.md):
- Both **erased dynamic syntax** (`fn first<T>(xs)`, `new Box(7)`) and **explicit/inferred type arguments** (`foo<int>(x)`, `new Box<int>(7)`) are accepted, with optional constraint bounds on `TypeParam`.
- Tree and VM execute generics polymorphically over runtime `Value`s with identical semantics.
- In LLVM AOT (`src/native/ir.rs`), scalar/known instantiations monomorphize up to bounded depth and fall back seamlessly to the generic `Value` entry whenever `any` or heterogeneous types appear.

---

## 3. Runtime Value Model & Memory Management

### 3.1 Value Layout & Reference Semantics
- **Compact Value Representation:** On Rust engines, `Value` uses an 8-byte atomically reference-counted immutable UTF-8 string (`ThinArcStr` in `src/value/mod.rs`) and boxed `Value::Result`, keeping the `Value` enum compact (16 bytes wide on 64-bit targets).
- **Reference Semantics:** Scalars (`int`, `float`, `bool`, `null`) copy by value; strings (`ThinArcStr`) copy via atomic refcount bump over immutable UTF-8 bytes; mutable collections (`list`, `map`, `set`, `bytes`, queues, trees, graphs, matrices, class instances) use shared heap handles (`Ref<T> = Arc<FastMutex<T>>`), so assignment aliases and `.clone()` deep-copies.
- **Single-Threaded Fast Path (`MULTITHREADED_ACTIVE`):** Before `spawn` is invoked, `FastMutex` uses a lock-free thread-local fast path with zero OS/atomic mutex lock contention per element access, permanently upgrading to thread-safe `Mutex` synchronization upon the first `spawn`. (On single-threaded `wasm32`, `is_multithreaded()` constant-folds to `false`.)
- **Native IR Scalar Unboxing & ValueV2:** The AOT backend (`src/native/ir.rs`) keeps pure integer/float locals and hoisted unboxed homogeneous rows (`list<int>`, `list<float>`, 2D row handles with loop-invariant bounds checks) in native LLVM registers (`i64`, `double`), bridging across runtime trampolines via the 16-byte `#[repr(C)] ValueV2 { tag: u64, payload: u64 }` representation (supporting inline scalars, 0..15-byte Small String Optimization, lazy string ropes, and heap pointers).

### 3.2 Deterministic Reference Counting + Cycle Collection
Memory reclamation guarantees are defined in [GC.md](GC.md):
- **Acyclic Objects:** Reclaimed deterministically and immediately when strong refcounts drop to zero. Deep singly/doubly linked object graphs and instance chains drain iteratively (`drop_iteratively`) to avoid host stack overflow.
- **Cycle Collection:** Self-referential containers, closures capturing their own environment, and cyclic class graphs are reclaimed automatically via **external-reference mark-sweep** (`external = strong_count - internal_edges`). Because external stack/environment references need no brittle root enumeration table, live Rust temporaries can never be prematurely swept.
- **Concurrency Safety:** Collection runs only when `gc_quiescent()` holds (`active_threads() == 1` — see [CONCURRENCY.md](CONCURRENCY.md)).
- **Explicit Escape Hatches:** `weak(r)` / `deref(w)` for manual weak links; `gc_collect()` and `gc_live_cells()` for manual inspection (note [ENGINE_CONTRACT.md](ENGINE_CONTRACT.md) Exemption #4 on engine-internal cell counts under native AOT arena handles).

---

## 4. Structured Error Handling & Exit Codes

### 4.1 Errors & Panics
- **Structural Failure Values:** `option` (`some`/`none`, `?.` / `??`) and `result` (`ok`/`err`) propagate expected recoverable failures explicitly.
- **Exceptions:** `throw <expr>;` unwinds cleanly to the nearest enclosing `try { ... } catch (e) { ... } finally { ... }`, preserving the thrown `Value` structurally—including across `spawn` / `.join()` task boundaries (`TaskOutcome::thrown` in `src/sched/mod.rs`).
- **Scope-Guard Safety:** `MutexGuardToken` and RAII active/blocked guards guarantee user mutexes and scheduler accounting unlock on early return, exception, or panic unwind.

### 4.2 Toolchain Exit Codes (`src/cli.rs`)
| Exit Code | Constant | Meaning |
| :--- | :--- | :--- |
| `0` | `EXIT_OK` | Success |
| `1` | `EXIT_USER_ERROR` | Program compile error, semantic error, config-pin error, or uncaught runtime error |
| `2` | `EXIT_USAGE` | CLI usage / driver / linker invocation error (`error[usage]`) |
| `4` | `EXIT_BYTECODE` | Corrupt, malformed, or SHA-256-tampered `.matrixc` bytecode / staticlib |

---

## 5. Multi-Engine Semantics & Slim Runtime Architecture

### 5.1 Engine Contract
Every engine (`tree`, `vm`, `native`, `wasm32`, and self-hosted `spid3r-self`) must satisfy [ENGINE_CONTRACT.md](ENGINE_CONTRACT.md): identical stdout, matching diagnostic `Phase` and first error line, identical exit code, and identical side effects within granted capabilities (with only the documented exemptions in `ENGINE_CONTRACT.md`).

### 5.2 Split Staticlib Linking (`core` vs opt-in stdlib groups)
In AOT builds (`src/native/ir.rs`, `src/stdlib/mod.rs`), compiled programs always register `spid3r_reg_core` and link **only** the opt-in subsystem initializer groups referenced by the program (`json`, `fs`, `net`, `db`, `media`, `tz`, `unicode`, `crypto`, `data`, `info`), allowing MSVC `/OPT:REF` / ELF `--gc-sections` to dead-strip unused subsystem code (SQLite/PostgreSQL/MySQL drivers, media encoders, IANA tzdata, Unicode tables).

---

## 6. Security & FFI Trust Model

- **Process Authority:** By default, a Spid3r script or AOT binary runs with full OS user authority ([SECURITY.md](SECURITY.md)).
- **Script Capability Filters:** `spid3r run --sandbox` / `exec --sandbox`, `Engine::deny(Capability::...)`, and `spid3r.ini` `[security]` policies (`security.basedir`, `security.disable_natives`, `modules.disable`) apply to interpreted/VM script execution using per-process immutable policy snapshots. They are **application-level policy filters**, not an OS kernel sandbox; AOT binaries always execute as native processes.
- **Native C FFI (`extern fn`):** Supported in AOT builds over `int` (`int64_t`), `float` (`double`), `bool`, `string` (`const char*`), `ptr` (opaque `void*`), and `usize` (`size_t`) ([FFI.md](FFI.md)). Rejected cleanly at compile time in Tree and VM engines.
- **Zero Compiler Network Access:** Compilation commands (`check`, `compile`, `build`, `fmt`, `doc`) perform zero network calls, and release artifacts verify SHA-256 checksums for `.matrixc` and `spid3r_lang` staticlibs.
