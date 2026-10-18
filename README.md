# Spid3r

> A small, dynamically typed language on a Rust host, with a tree-walking reference
> interpreter **and** a bytecode compiler + stack VM — verified to behave identically.

Spid3r reads like plain English keywords (`fn`, `let`, `class`, `spawn`, `import`) but has
real semantics: dynamic types, closures with upvalues, single-inheritance classes with
interfaces, structured error handling (`try`/`catch`/`finally`/`throw`), modules with
public/private visibility, task-based concurrency, a standard library, gradual type
annotations, and 20+ builtin data structures backed by real nodes/heaps.

The crate is pure Rust with **no third-party dependencies** and no runtime requirements.

**Current status:** language v1.0 complete. **250 Rust tests, 0 failed**; all 23 shipped
examples run with identical output on the tree interpreter, the bytecode VM, and
`compile`+`exec`. The 2026-10 independent audit closed the last known engine divergences
(VM closure/upvalue semantics) and two packaging security issues; every fix carries a
regression test (see *Testing* below and `AUDIT_REPORT.md`).

---

## Contents

- [Build](#build)
- [A 30-second tour](#a-30-second-tour)
- [The toolchain](#the-toolchain)
- [The language](#the-language)
  - [Values, types & truthiness](#values-types--truthiness)
  - [Operators & precedence](#operators--precedence)
  - [Control flow](#control-flow)
  - [Functions & closures](#functions--closures)
  - [Classes & interfaces](#classes--interfaces)
  - [Error handling](#error-handling)
  - [Strings](#strings)
  - [Data structures](#data-structures)
  - [Gradual typing](#gradual-typing)
- [Concurrency](#concurrency)
- [The bytecode VM & `.matrixc`](#the-bytecode-vm--matrixc)
- [Projects & dependencies](#projects--dependencies)
- [Tooling: fmt · bench · debug · gui · native](#tooling)
- [Memory model & GC](#memory-model--gc)
- [Environment variables](#environment-variables)
- [Testing](#testing)
- [Examples](#examples)
- [Docs](#docs)
- [Known limitations](#known-limitations)

## Build

Requires a recent Rust toolchain (edition 2021). From the crate root:

```sh
cargo build                 # debug binary   → target/debug/spid3r(.exe)
cargo build --release       # optimized      → target/release/spid3r(.exe)
cargo test                  # unit + integration + differential tests
```

Optional feature-gated backends (see `docs/BUILDING.md`):

```sh
cargo build --features native   # LLVM AOT backend (needs LLVM 18.1+)
cargo build --features gui      # desktop webview GUI (needs the platform webview)
```

## A 30-second tour

```text
// hello.matrix — the smallest Spid3r program.
fn main() {
    println("hello");
    return 0;
}
```

```sh
$ spid3r run hello.matrix
hello
$ spid3r run --engine vm hello.matrix      # same output on the bytecode VM
hello
$ spid3r compile hello.matrix              # writes hello.matrixc (self-contained)
$ spid3r exec hello.matrixc                # runs the bytecode, no source needed
hello
```

Functions, closures, collections, classes, and error handling:

```text
class Animal {
    public let name;
    private let noise;
    fn new(name, noise) { self.name = name; self.noise = noise; }
    public fn speak(self) -> string { return self.name + " says " + self.noise; }
}

fn main() {
    let adder = fn(x) { return x + 100; };       // closures capture their binding
    println(adder(5));                           // 105

    let orders = stack();
    orders.push("review"); orders.push("merge");
    println(orders.pop());                       // merge

    println(new Animal("Rex", "woof").speak());  // Rex says woof

    try { print(10 / 0); } catch (e) { println("caught: " + e); }
    return 0;
}
```

## The toolchain

```
spid3r run [--engine tree|vm] <file.matrix> [args…]
                                               compile + run; args after the file become
                                               the program's args()
spid3r compile <file.matrix> [-o out] [--dump]
                                               write bytecode (.matrixc); --dump prints
                                               the disassembly
spid3r exec <file.matrixc>                     run bytecode on the VM (imports embedded)
spid3r check <file.matrix>                     lex + parse + semantic check only
spid3r build <file.matrix> [-o out] [--target triple] [--release]
                                               native machine-code executable (native
                                               feature; needs LLVM 18.1+)
spid3r gui <file.matrix> [--html page.html] [--dry-run]
                                               run in a webview window (gui feature)
spid3r test                                    run every examples/*.matrix in all three
                                               engines and compare
spid3r bench [--engine tree|vm] [--runs N] [--compare]
                                               time benchmarks/*.matrix, best of N
spid3r fmt [--check] [files...]                canonical formatting; --check exits 1
                                               when a file differs
spid3r debug <file.matrix> [--break L[,L…]] [--commands FILE]
                                               line debugger: breakpoints, step/next/
                                               finish, backtrace, locals
spid3r init [dir]                              scaffold spider.toml + src/main.matrix
spid3r install                                 vendor path/git deps into vendor/, write
                                               spider.lock (commit-pinned, SHA-256)
spid3r dist [-o out] [--no-opt]                compile the manifest entry into a
                                               self-contained target/<package>.matrixc
spid3r pack [-o out.tar]                       archive spider.toml + src/ into
                                               dist/<name>-<version>.tar; prints the
                                               archive hash and sha256(tree)
spid3r unpack <archive> [dir]                  extract a packed project (guarded)
spid3r repl                                    interactive prompt
spid3r version | help
```

Exit codes: `0` success · `1` user-program error · `2` usage error.

Two execution engines share the same front end (lexer → parser → semantic checker):

- **`tree`** (default) — the tree-walk interpreter. The reference implementation.
- **`vm`** — compiles the checked AST to bytecode and runs a stack machine.
  Semantically identical to `tree`; an optimizer runs by default (`--no-opt` disables).

## The language

### Values, types & truthiness

Primitives: `int` (64-bit signed), `float` (64-bit IEEE), `bool`, `string` (immutable
UTF-8), `null`. Everything else is a builtin object. `type_of(x)` names any value.

| Value | Truthy when |
|---|---|
| `bool` | its own value |
| `null` | never |
| `int` / `float` | non-zero |
| `string` | non-empty |
| `option` | `some(…)` |
| containers, `result`, tasks, instances | always true |

Numeric notes: `int + float` promotes to `float`, but `int / int` truncates toward zero
(`7 / 2` is `3`). Division or modulo by zero throws. Integer arithmetic **wraps** on i64
overflow. `1 == 1.0` is true. String escapes: `\n`, `\t`, `\\`, `\"`.

### Operators & precedence

`+ - * / %` · `== != < <= > >=` · `and or not` (short-circuit) · assignment
`= += -= *= /= %=` (right-associative) · postfix `call` / `xs[i]` / `.field` /
`.method()` / `m[k] = v`. Precedence, tightest first: postfix → unary
(`-`, `not`, `await`, `spawn`) → `* / %` → `+ -` → comparisons → `and` → `or` →
assignment. There is no `!`, `++`, or `**` — use `not`, `x += 1`, and `pow(b, e)`.

### Control flow

`if / elif / else`, `while`, and two `for` forms — collection iteration
(`for x in [1, 2, 3]`) and C-style (`for let k = 0; k < n; k = k + 1`) — plus
`break` / `continue`. Lists, vecs, tuples, deques, ring buffers, stacks, queues, sets,
bags, map values (insertion order), matrix cells, graph nodes, and string characters are
all iterable.

### Functions & closures

Functions are first-class; `fn(x){…}` literals nest and capture bindings.

Capture rules (identical on both engines, pinned by regression tests):

- **Mutation is visible** through the shared binding.
- **`for x in …` rebinds per iteration** — closures made in iteration N see that
  iteration's value (`0, 1, 2`).
- **`while` and C-style `for` share one binding** — closures there observe the final
  value; bind a fresh `let` inside the body for snapshots.
- **Every activation is isolated** — recursive functions and factories never leak state
  between calls.

Quirk: a `fn` declaration *inside a block* currently parses as an expression statement
and needs a trailing `;` — prefer `let f = fn(x) { … };` for local functions. Top-level
`fn` declarations never need a semicolon.

### Classes & interfaces

Single inheritance (`extends`), interfaces (`implements`), explicit `self` receiver, and
`public`/`private` members (**private is the default**; the checker rejects out-of-class
access). The constructor is the `new` method, invoked with `new Class(...)`. Methods
resolve through the parent chain, so overrides dispatch polymorphically.

### Error handling

`throw` raises any value; `try` / `catch (e)` / `finally` handle it (`finally` always
runs; at least one of catch/finally is required). Uncaught errors print position and a
call-stack trace, then exit 1. Expected failure uses sum types:

- `option` — `some(v)` / `none`: `is_some`, `is_none`, `unwrap`, `unwrap_or(d)`.
- `result` — `ok(v)` / `err(e)`: `is_ok`, `is_err`, `unwrap`, `unwrap_or(d)`, `unwrap_err`.

File and process builtins (`read_file`, `write_file`, `spawn_process`) return `result`
values rather than throwing — branch with `is_ok`/`is_err`.

### Strings

Immutable UTF-8; lengths and indexes are in characters. Method suite: `len`, `is_empty`,
`slice(a[, b])`, `chars`, `reverse`, `upper`, `lower`, `trim`, `ltrim`, `rtrim`,
`contains`, `starts_with`, `ends_with`, `index_of`, `split`, `replace`, `repeat`,
`pad_left`/`pad_right`, and `format` (`"a={} b={}".format(1, "two")`).

### Data structures

Constructors are ordinary globals: `list`, `vec`, `deque`, `tuple`, `ring_buffer`,
`stack`, `queue`, `priority_queue`, `map`, `set`, `bag`, `hash_table`, `linked_list`,
`singly_linked_list`, `doubly_linked_list`, `binary_tree`, `bst`, `trie`, `heap`,
`matrix`, `graph`, plus shared state (`ref`, `mutex`, `channel`). They share a common
verb set (`len`, `is_empty`, `clear`, `push`/`pop`/`peek`/`get`/`contains`) and misuse
throws a descriptive, catchable error.

Highlights:

- `map` keeps insertion order; `m[k]` reads and `m[k] = v` assigns.
- Lists carry **higher-order methods** — `map`, `filter`, `reduce`, `for_each`, `all`,
  `any` — over a snapshot, so callbacks can't corrupt the iteration.
- Trees/graphs/heaps: `bst` (in/pre/post-order traversal, `find_closest`, `get_ith`),
  `graph` (`bfs`, `dfs`, `path_exists`, `find_path`, `cycle`), `heap`
  (`change_priority`, `merge`), `matrix[r][c]` read/assign.

### Gradual typing

Annotations are optional everywhere and enforced when present — `let x: int`, parameter
and return annotations (`fn add(a: int, b: int) -> int`), and class fields.
`int` widens to `float`; `any` opts out. Mismatched assignments, call arguments, returns,
and field writes are rejected at compile time with positions.

## Concurrency

`spawn fn(){…};` runs a task on a threaded scheduler and returns a `task`. `await` joins a
task or receives from a channel — only inside an `async fn`. Shared state: `channel()`
(unbuffered), `mutex(v)` (RAII guard via `lock()`), `ref(v)` (interior-mutable cell).

```haskell
async fn settle(t) { return await t; }

fn main() {
    let t = spawn fn() { return 7; };
    println(settle(t));                        // 7

    let bad = spawn fn() { throw "task-error"; };
    sleep_ms(30);
    println(bad.status());                     // failed
    try { bad.join(); } catch (e) { println("caught " + e); }
    return 0;
}
```

Task API: `t.status()` (`"running"` / `"done"` / `"failed"`), `t.join()` (repeatable; a
failed task re-raises its error), `t.is_done()`. Channels: `send`, `recv`, `try_recv`,
`state`, `len`. Also: `task_count()`, `sleep_ms(n)`, `yield()`,
`spawn_process(cmd, args)` (synchronous, `ok(exit_code)` / `err(msg)`, no shell).

## The bytecode VM & `.matrixc`

The compiler lowers the checked AST into a clox-style `Chunk` (code bytes + constant,
function, and class pools + per-instruction spans). The VM is a register-free stack
machine whose frames double as operand stack and locals; captured locals route through
per-activation heap cells, so closures stay correct across recursion, loops, and spawned
tasks. Every value operation is delegated to the shared `value::collections` layer, which
is what keeps the engines identical.

- `compile` writes a **self-contained** `.matrixc`: imported modules are resolved,
  checked, and embedded keyed by resolved path — `exec` needs no source on disk.
- Chunks are **validated** before execution: safe opcode decoding (no `transmute`),
  rejection of truncated instructions, out-of-range pool references, and misaligned jump
  targets. Bad bytecode becomes a clean runtime error, never a panic.
- The **optimizer** (on by default, `--no-opt` to disable) folds constants through the
  interpreter's own helpers (`1/0` is never folded), applies peephole rewrites, folds
  static branches, and eliminates dead code (skipped inside `try` chunks).
- Full format and design notes: `docs/BYTECODE.md`.

## Projects & dependencies

`spid3r init myproj` scaffolds `spider.toml` + `src/main.matrix`.

```toml
[package]
name = "myproj"
version = "0.1.0"
entry = "src/main.matrix"

[dependencies]
mathlib = { path = "../mathlib" }                            # path dependency
extras  = { git = "https://example/extras", rev = "main" }   # git dependency
serde   = { version = "^1.0" }                               # local registry
```

- `spid3r install` vendors dependency sources into `vendor/<name>/` and writes a
  deterministic `spider.lock` (git deps pinned to the resolved commit, SHA-256 tree hash
  per dependency). Idempotent; tampered or stale `vendor/` trees are re-synced.
- Version constraints: `^1.0` (same major), `>=1.2`, `=1.2.3`, exact — checked against
  the dependency's own manifest.
- **Local registry:** `{ version = "…" }` resolves from `$SPID3R_REGISTRY`, an offline
  directory of `<name>/<version>/` packages; `dist/sha256.txt` is verified against the
  tree hash that `spid3r pack` prints.
- **Workspaces:** `[workspace] members = […]` share one `vendor/` + lockfile with
  name-conflict detection.
- **Distribution:** `spid3r dist` builds a self-contained `.matrixc` from the manifest
  entry; `pack`/`unpack` move source projects as deterministic, path-traversal-guarded
  tarballs.
- Inside a project: `run` (no file) runs the manifest entry; `test` differentially runs
  `tests/*.matrix` per workspace member (use the `assert`/`assert_eq` builtins).

There is no remote/HTTP registry and registry packages' own dependencies are not resolved
transitively — declare what you use. Details: `docs/REGISTRY.md`.

## Tooling

- **`spid3r fmt`** — canonical formatting in place; machine-checked invariants (re-lexes
  to the identical token stream, idempotent), comment/literal preservation, line
  wrapping. `--check` exits 1 on any difference.
- **`spid3r bench`** — best-of-N timings per engine; `--compare` also asserts
  tree == vm output, so benchmarks double as differential tests.
- **`spid3r debug`** — line debugger over the VM: `--break 3,7`, `step`/`next`/`finish`,
  `backtrace`, `locals`, source listing; scriptable via `--commands FILE`.
- **`spid3r gui`** (`gui` feature) — desktop webview window. Program side:
  `gui_set_html(html)`, `gui_eval(js)`, `gui_on(event, handler)`, `gui_emit(event,
  payload)`; page side: `window.spid3r.emit(event, payload)`. `--dry-run` replays the
  whole bridge headlessly (what the tests drive). See `docs/GUI.md`.
- **`spid3r build`** (`native` feature) — LLVM AOT: control flow becomes machine code and
  runtime operations go through trampolines that reuse the exact VM/value code, so native
  output matches the interpreter (22/23 examples; a class declared inside a function is
  rejected with a clear diagnostic). See `docs/NATIVE.md`.

## Memory model & GC

Values are reference-counted (`Arc`/`Mutex` in both engines, so they cross `spawn`
boundaries). Acyclic garbage is reclaimed eagerly. **Cycles** — a list containing itself,
an instance pointing at itself — are collected for `list`/`vec`, `set`, `map`, and
instances, automatically at statement boundaries (`SPID3R_GC=0` disables);
`gc_collect()` forces a pass and `gc_live_cells()` inspects. Cycles through other
container types still leak until collection is widened — use `weak(v)` / `deref(w)` for
those back-edges. Design: `docs/GC.md`.

## Environment variables

| Variable | Effect |
|---|---|
| `SPID3R_PATH` | extra module search directories (after the importing file's directory) |
| `SPID3R_REGISTRY` | offline registry directory for `{ version = "…" }` dependencies |
| `SPID3R_GC` | `0` disables statement-boundary automatic cycle collection |

## Testing

Correctness is "the two engines agree", so Spid3r is verified differentially:

- `spid3r test` — every `examples/*.matrix` runs three ways (tree, vm, compile+exec) and
  all three stdout streams must match; libraries are semantic-checked. **23 ok, 0 failed.**
- `cargo test` — **250 tests, 0 failed**: unit tests per subsystem, integration tests,
  the differential harness (`tests/difftest.rs`), and 14 dedicated engine-parity
  regressions (`tests/audit_regressions.rs`) covering closure capture, per-iteration loop
  bindings, scope-exit slot release, upvalue index offsets, map index assignment, file-op
  results, the task API, JSON round-trips, and RNG bounds.
- `difftest.sh` — standalone shell version of the three-way comparison.
- The 2026-10 audit added a 27-program probe suite (`audit/probes/`) that pins edge
  semantics across all three engines; its findings and fixes are summarized in
  `AUDIT_REPORT.md`.

## Examples

All runnable from the project root: `spid3r run examples/<name>.matrix` (add
`--engine vm` to compare engines).

| | | |
|---|---|---|
| `hello` · smallest program | `variables` · types & promotion | `conditions` · if/elif/else |
| `loops` · all three forms | `functions` · recursion & closures | `higher_order` · map/filter/reduce |
| `list` `map` `set` · core collections | `stack` `queue` `heap` · ordered | `graph` `bst` `linked_list` · structures |
| `class` `interface` · OOP | `error_handling` · try/catch, result | `channel` `tasks` · concurrency |
| `modules` + `math_util` · imports | `server_report` · larger program | `gui/counter` · webview UI |

## Docs

- [`doc.html`](doc.html) — the full developer documentation (self-contained page).
- [`docs/GRAMMAR.md`](docs/GRAMMAR.md) — EBNF + precedence + parser rules
- [`docs/KEYWORDS.md`](docs/KEYWORDS.md) — the reserved-word registry
- [`docs/DATA_STRUCTURES.md`](docs/DATA_STRUCTURES.md) — builtin containers and methods
- [`docs/STANDARD_LIBRARY.md`](docs/STANDARD_LIBRARY.md) — global functions
- [`docs/BYTECODE.md`](docs/BYTECODE.md) — `.matrixc` format, VM design, concurrency
- [`docs/GC.md`](docs/GC.md) — cycle-collector design
- [`docs/NATIVE.md`](docs/NATIVE.md) — LLVM AOT backend
- [`docs/GUI.md`](docs/GUI.md) — webview GUI layer
- [`docs/REGISTRY.md`](docs/REGISTRY.md) — local dependency registry
- [`docs/BUILDING.md`](docs/BUILDING.md) — building from source (incl. LLVM)
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — phases and honest limitations
- [`AUDIT_REPORT.md`](AUDIT_REPORT.md) — 2026-10 independent audit: findings, fixes, QA status

## Known limitations

Honest gaps, in priority order:

- **Local `fn` declarations inside blocks** parse as expression statements and need a
  trailing `;` (bind with `let f = fn(x) { … };` instead); the parser also lacks a
  recursion depth cap, so pathological nesting aborts the process rather than erroring.
- **Cycle GC coverage** — trees/graphs/heaps/matrices cycles are not traced yet; use
  `weak`/`deref` for those back-edges.
- **Native AOT subset** — a class declared inside a function is rejected (clean
  diagnostic); everything else (`try`/`catch`/`throw`, `import`, closures, classes,
  `spawn`/`async`/`await`) compiles and matches the interpreter on 22/23 examples.
- **Local-only dependencies** — no remote/HTTP registry; registry packages' own
  dependencies are not resolved transitively.
- **Lexer edges** — no exponent float literals (`1.5e3` mis-lexes),
  `-9223372036854775808` is unrepresentable as a literal, overflowing float literals
  become `inf`, and a `\` at end-of-line inside a string reports a malformed diagnostic.
- **Tar archives** — no ustar long-name/prefix support and no header-checksum
  verification; entries over 100 bytes truncate.
- **Design choices, documented:** integer overflow wraps silently; `int / int` truncates;
  `err(...)` is truthy; comparison chains are left-associative; assignments are legal in
  `if`/`while` conditions.

---

**Spid3r** — language v1.0. Pure Rust, no runtime dependencies. Source in `src/`;
interactive docs in [`doc.html`](doc.html).
