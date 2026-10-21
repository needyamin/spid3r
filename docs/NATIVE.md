# Spid3r native AOT backend (LLVM)

This document describes the `native` backend: `spid3r build` compiles a
`.matrix` program all the way to a standalone machine-code executable, the
same way `rustc` builds a binary. It is gated behind the `native` Cargo
feature (adds `inkwell` and requires LLVM 18.1+ — see
[BUILDING.md](BUILDING.md)). The core toolchain itself stays
zero-dependency.

Everything here is implemented in `src/native/` (`rt.rs`, `ir.rs`, `llvm.rs`,
`link.rs`) and verified against the reference engines — the differential
numbers at the bottom are measured, not estimated.

## Quick start

```
cargo build --release --features native
./target/release/spid3r build examples/hello.matrix -o hello
./hello
```

`spid3r build` reports what it produced:

```
built hello (x86_64-pc-windows-msvc 64 bit, debug)
```

Flags: `-o <path>` (default: source stem + platform extension), `--target
<triple>` (default: host triple), `--release` (LLVM `Aggressive` optimizations
instead of `Default`).

Without the feature, `spid3r build` fails with a usage error pointing at
BUILDING.md rather than a confusing linker error.

## Architecture: shared-runtime IR

The backend has two halves:

- **Runtime** (`src/native/rt.rs`, always compiled, zero dependencies): the
  `extern "C"` trampolines plus `spid3r_rt_start`. These live inside the
  `staticlib` the crate produces (`libspid3r_lang.a` / `spid3r_lang.lib`) and are what
  AOT-compiled binaries call into.
- **Codegen** (`src/native/ir.rs` + `llvm.rs` + `link.rs`, behind the
  `native` feature): AST → LLVM IR → object file → linked executable.

The guiding idea: **user control flow becomes real machine code; every
runtime semantic routes through a trampoline that delegates to the exact same
Rust code the tree and VM engines use.** There is one implementation of the
language's semantics, not three, which is why native output is byte-identical
to `run --engine tree` on every supported program (see Verification).

```
hello.matrix ── lex / parse / semantic ──▶ AST
                                            │  ir.rs
                                            ▼
                                   LLVM IR module
                                            │  llvm.rs (TargetMachine, PIC)
                                            ▼
                                    object file (.o)
                                            │  link.rs  (+ libspid3r_lang.a)
                                            ▼
                              standalone native executable
```

What compiles to machine code: functions (including nested functions and
closures), branches, all three loop forms, calls. What goes through
trampolines: arithmetic and comparison, truthiness/equality, string
conversion and display, calls, global variable access, list/map/set
construction and indexing, `for-in` iteration, class construction, method
dispatch, field access, upvalue cells, and the `print`/`println`/`eprintln`
builtins.

## Compiled function ABI

Every function — top-level, nested, closure, method, or constructor —
compiles to the same shape:

```llvm
define ptr @spid3r_fn.N(ptr %env, i64 %argc, ptr %argv) -> ptr
```

- Every value is an opaque `*mut Value` handle (one machine word). Values are
  boxed and **leaked** (see Memory model).
- `argv` points at `argc` consecutive pointers (the codegen materializes an
  `[argc x ptr]` array in LLVM memory; a null pointer when there are no
  arguments). The runtime rebuilds a `Vec<Value>` from it.
- `%env` is the upvalue environment (an array of cells) or null for functions
  that capture nothing.
- The generated `main` is a **C-style `main`**: it calls
  `spid3r_rt_start(entry)`, which installs the stdlib, runs the entry function
  (class registration + top-level statements), then calls `main` and uses its
  return value as the process exit code (`Int` → exit code, anything else → 0)
  — matching `run --engine vm`.
- Constructors take the implicit `self` at `argv[0]` and their parameters from
  `argv[1]`; regular methods declare `self` explicitly at `argv[0]`.

## Runtime trampolines (the C ABI generated code calls)

| Group | Trampolines |
| --- | --- |
| Values | `spid3r_value_int` / `_float` / `_bool` / `_null` / `_str` |
| Operators | `spid3r_binop`, `spid3r_unop`, `spid3r_truthy`, `spid3r_eq`, `spid3r_display` (+ `spid3r_cstr_free`) |
| Calls | `spid3r_call` (compiled fns, stdlib natives, class construction), `spid3r_make_fn`, `spid3r_construct` |
| Globals | `spid3r_global_get` / `_set` / `_def` / `_exists` |
| Collections | `spid3r_construct`, `spid3r_index_get` / `_set`, `spid3r_len`, `spid3r_forin_start` / `_next` / `_end` |
| Closures | `spid3r_cell_new` / `_get` / `_set`, `spid3r_env_alloc` / `_slot` / `_set_slot` |
| Classes | `spid3r_class_register` / `_add_field` / `_add_method` / `_set_ctor`, `spid3r_field_get` / `_set` |
| I/O | `spid3r_print`, `spid3r_println`, `spid3r_eprintln` |
| Process | `spid3r_rt_start`, `spid3r_abort`, `spid3r_runtime_version` |

Builtins such as `read_line`, `env`, `str`, `sqrt`, or file I/O are ordinary
`Value::Native` stdlib functions: generated code looks them up as globals and
calls them through `spid3r_call`, which dispatches to the same stdlib the
interpreters use. `read_line` returns `Null` at EOF, so stream-processing
loops (`while line != null`) terminate identically under stdin redirection.

## Supported subset

Supported today: functions/closures with upvalues, `let`/`const`,
`if`/`elif`/`else`, `while`, C-style `for`, `for-in`,
`return`/`break`/`continue`, `try`/`catch`/`finally`/`throw` (see
[Exceptions](#exceptions)), `import` (see [Modules](#modules)),
`spawn`/`async`/`await` + channels/mutexes (see [Concurrency](#concurrency)),
top-level classes and interfaces (fields,
methods, constructors, `implements`), inheritance with `super(...)`
/ `super.m(...)` (parent constructor on the same instance, parent method on
the same receiver — resolved like tree/vm, `spid3r_super_call`), method calls,
field access, indexing, list construction, the standard collections (`list`,
`map`, `set`, `queue`, `deque`, `stack`, `heap`) via their builtin
constructors, and all stdlib builtins.

Explicitly rejected with a clear diagnostic (never silently mis-compiled):

| Construct | Diagnostic |
| --- | --- |
| class inside a function | `'class declaration inside a function' is not supported ...` |

These are compile-time `error[usage]` diagnostics (exit code 2), the same
phase the CLI uses for other usage errors.

### Concurrency

`spawn`, `await`, channels, and mutexes work in compiled code by reusing the
same shared worker pool the tree/VM engines use (`crate::sched::Scheduler`).
`spawn f` queues a job that invokes the compiled closure on a worker thread
(`spid3r_spawn`); `await` joins a task or receives from a channel
(`spid3r_await`). Values are `Arc<Mutex>` and the globals table is a shared
mutex, so crossing the thread boundary is the same thread-safety the other
engines rely on; an uncaught `throw` inside a task becomes the task's failure
and surfaces at `await` as a "task failed: …" error. `channel.matrix` and
`tasks.matrix` both match the interpreter exactly.

### Exceptions

`try`/`catch`/`finally`/`throw` are implemented without native unwinding (and
without `setjmp`/`longjmp`, which libc does not expose on Windows): an
operation that would throw records its value in a thread-local "pending throw"
flag and returns a placeholder, and the generated code tests the flag right
after each operation, branching to the enclosing `try`'s handler — compiled
control flow, so a `throw` (inline, from an operation such as "division by
zero", or from a nested compiled call) skips the rest of the region exactly
like the tree/VM engines. Operation errors (e.g. `unwrap` on `none`) become
catchable string exceptions. A `throw` with no active `try` propagates out via
the function call chain and is reported as an uncaught throw at the top level
(exit code 1); the interpreter's per-instruction call-stack trace is the one
diagnostic AOT cannot reproduce.

### Modules

`import "name";` is resolved and the module is **flattened into a single
program** ahead of codegen (`native::modules::flatten_imports`): each imported
module's top-level declarations are inlined once (transitively, deduped by
canonical path), so `build_module` compiles one self-contained module with no
runtime import window — the AST-level analogue of the bytecode path embedding
imported modules into `.matrixc`. Resolution mirrors the bytecode path (the
importing file's directory, then `SPID3R_PATH`); circular imports are reported
and missing modules produce a clean "module not found" diagnostic. Visibility
is already enforced by the semantic pass, which runs before flattening in the
`build` flow, so only public names may be imported.

## Memory model

- **No garbage collector.** Every value is a boxed allocation that lives for
  the whole process (an arena by another name). Memory use tracks *total*
  allocations, not the live set. This is a deliberate trade: it keeps the
  runtime dependency-free and makes semantics trivially identical across all
  three engines. Long-running stream processors stay flat because per-line
  garbage is the only thing that accumulates (verified: a 100,000-line stdin
  stream processes in ~1.4 s).
- **Bounded recursion.** `MAX_CALL_DEPTH = 1000` (the same constant the VM
  and tree interpreter use) is enforced by a thread-local counter in
  `spid3r_call`, `spid3r_method`, and constructor construction. Hitting it
  aborts with the interpreters' exact diagnostic:
  `stack overflow: call depth limit reached` — instead of a silent native
  stack overflow.
- **Native stack.** MSVC links pass `-stack:4194304` (4 MiB) to give the
  depth limit headroom, especially in debug builds where every local lives in
  its own stack slot.
- **Runtime failures abort cleanly**: `spid3r_abort` prints
  `error[runtime]: <message>` to stderr, flushes stdout first, and exits with
  code 70. Because the program's `main` is C-style, Rust's flush-on-exit
  never runs — so `spid3r_rt_start` (success) and `abort` (failure) both flush
  stdout explicitly. Output is never lost on the failure path.

## Exit codes

| Situation | Code |
| --- | --- |
| Success (`main` returns Int `n`) | `n` |
| Success (`main` returns non-Int) | 0 |
| Parse / semantic error | 1 |
| Unsupported construct, bad usage | 2 |
| Runtime abort (type error, missing field, depth limit, …) | 70 |

## Environment variables

| Variable | Effect |
| --- | --- |
| `SPID3R_DUMP_IR` | Print the generated LLVM IR to stderr before emitting the object |
| `SPID3R_LLVM_LIB_DIR` | Directory holding `LLVM-C.lib` / `libLLVM-18.so`; also read by `build.rs` |
| `SPID3R_LLVM_BIN_DIR` | Windows: directory holding `LLVM-C.dll`; `build.rs` copies it next to the built binaries so `cargo run`/`cargo test` work without touching PATH |
| `LLVM_SYS_181_PREFIX` | llvm-sys convention prefix; its `lib/` is searched as a fallback |

## Cross-compilation

`--target` accepts any LLVM triple; the host default comes from
`default_triple()` (x86_64 / aarch64 / i686 × `pc-windows-msvc`,
`apple-darwin`, `unknown-linux-gnu`). The target machine uses a per-triple
CPU (`x86-64`, `apple-a14`, `generic-rv64`, `generic`), PIC, and the default
code model. The runtime staticlib itself is built for the host, so
cross-*OS* builds need a toolchain built for that target; changing the triple
for same-OS cross-arch (e.g. pinning i686 on x86_64 Windows) works when the
platform linker supports it.

## Verification

The backend is held to a strict standard: **native output must be
byte-identical to the tree interpreter.**

- `tests/native_tests.rs` (runs under `cargo test --features native`):
  builds every example natively and diffs stdout against the tree engine, and
  asserts unsupported constructs are rejected with the diagnostics above. It
  skips cleanly when `LLVM-C.dll` is absent next to the test binary.
- `tools/native_diff.sh`: the same differential as a shell runner over
  `examples/*.matrix`. Every step (VM reference run, native build, native
  run) is wrapped in a timeout and reported as `HANG` if it ever stalls.
  Current result: **22 pass / 0 fail / 1 skip** — the one skip is `math_util`,
  a no-`main` library example (not a native gap; there is no program to run).
- Spot checks, all identical to the tree interpreter: `fib(10)` = 55,
  fizzbuzz(15), `sum_to(999)` = 499500, a 1,000,000-iteration spin =
  499999500000, nested 1000×1000 loops = 249500250000, and a 100,000-line
  stdin stream (~1.4 s).
- `spid3r test` additionally checks `tree == vm == exec` for every example.
