# Embedding Spid3r in Rust applications (API v0)

Spid3r's core is a zero-dependency Rust library. `spid3r_lang::embed` is the
supported surface for hosting it as a scripting layer, sized for Rust. The API
is semver-tracked from this document forward; breaking changes to
`spid3r_lang::embed` will bump the crate minor version.

> **Trust model.** An embedded script runs with the authority of the process
> that hosts it — files, processes, sockets, environment. That is the same
> default contract the Lua C API has, which is exactly why the case that needs
> isolation needs an explicit decision. `Engine::deny(...)` removes the built-in
> natives for the four broad capabilities; it is a policy filter, not a sandbox.
> Read `docs/SECURITY.md` before running a script you did not write.

## The whole host-side integration (~20 lines)

```rust
use spid3r_lang::embed::{conv, Engine};

let engine = Engine::new()
    .register("shout", |args| {
        let s = conv::as_str(args.first().unwrap_or(&spid3r_lang::Value::Null)).unwrap_or("");
        Ok(conv::from_string(s.to_uppercase()))
    })
    .set_global("version", conv::from_string("1.0"));

let out = engine
    .run("demo", r#"fn main() { println(shout("hi") + " " + version); return 0; }"#)
    .expect("script failed");
assert_eq!(out.exit_code, 0);
assert_eq!(out.output, "HI 1.0\n");
```

A runnable version ships as an example: `cargo run --example embed_hello`.

## API reference

### `Engine` (builder; cheap to clone)

| Method | Effect |
|---|---|
| `Engine::new()` | Standard module search path (`modules::default_module_dirs()`) |
| `.register(name, f)` | Host function callable from Spid3r. `f: Fn(&[Value]) -> Result<Value, String>`; `Err` becomes a catchable runtime error. State: capture `Arc<...>` clones (see below). Overrides same-named stdlib natives and prior registrations |
| `.set_global(name, value)` | Global binding visible to the script. Dynamic (`Any`): callable/readable, unchecked. Shadowable by script `let` (overwrites the host binding) |
| `.add_module_dir(dir)` | Extra import search root, appended after the defaults |
| `.deny(capability)` | Replace the built-in natives of a `Capability` (`FileSystem`, `Process`, `Network`, `Environment`) with stubs that raise a catchable `capability denied` error. A policy filter, not a sandbox — see `docs/SECURITY.md` |
| `.deny_all()` | Shorthand for all four capabilities |
| `.budget(steps)` | Cap the work one run may do. After roughly `steps` units (statements in the tree engine) the run fails with the **fatal** error `execution budget exhausted`, which the script's `try`/`catch` cannot intercept. Per run, shared with spawned tasks; `None` (default) is unlimited. See `docs/SECURITY.md` |
| `.set_script_args(vec)` | `args()` for the script. **Process-global** (shared with the CLI); set once per process |
| `.run(name, src)` | Parse → check → execute on a dedicated big-stack thread → `Result<RunResult, ScriptError>`. `RunResult { exit_code, output }`: `main`'s return value and the captured `print`/`println`/`eprint` text of the main thread |
| `.run_source(name, src)` | Alias of `run` returning only the exit code |
| `.run_source_captured(name, src)` | Alias of `run` returning `(exit_code, output)` |

`ScriptError { diagnostic, output }` — the phase diagnostic plus whatever was
printed before the failure (empty for lex/parse/check failures).

### `conv` — value conversions

| Rust → Spid3r | Spid3r → Rust |
|---|---|
| `from_i64`, `from_f64`, `from_bool` | `as_i64` (`int`), `as_f64` (`float`, `int` widens), `as_bool` |
| `from_string(impl Into<Arc<str>>)` | `as_str(&str)` |
| `from_list(Vec<Value>)` | `as_list(&[Value])` (snapshot) |
| — | `display(&Value)` (what `println` would print) |

Every accessor returns `Option`; nothing guesses.

## Semantics and limits (v0)

* **Engine**: the tree-walk interpreter (the reference engine) executes
  embedded scripts; the bytecode VM stays a toolchain/CLI concern for now.
  Each run executes on a dedicated 512 MiB-stack worker, and the program runs
  on that thread (`run_program_inline`), so thread-local output capture sees it.
* **Host names are dynamic (`Any`)**: the checker (via
  `semantic::check_file_with`) knows they exist — callable, assignable — but
  their types are unchecked, matching the stdlib natives' treatment. The
  gradual-typing contract is unchanged for annotated code.
* **Host functions are `Fn`**: for mutable state use interior mutability
  (`Arc<AtomicI64>`, `Arc<Mutex<_>>`). They work as collection callbacks too
  (`xs.map(host_fn)` — accepted by the same callback host as user functions).
* **Output capture is per-thread** (thread-local buffer): main-thread
  `print`/`println`/`eprint` are captured; output from spawned tasks still
  goes to the process stdout. Concurrent engines are isolated from each other.
* **Script args are process-global** (matching the CLI). Concurrent engines
  that set different args race; set once per process.
* **Module resolution**: the script's `name` is its virtual file path (its
  parent directory is searched first), then the standard path, then
  `add_module_dir` roots (searched by `semantic::public_names_of_in` at check
  time and by the loader at run time — one shared search order).

## Verification

* `tests/embed_tests.rs` — 10 integration tests (exit codes, capture, host
  callbacks incl. collection callbacks, stdlib override, shadowing, module
  resolution from host dirs, deep-data runs on the worker, capture isolation
  between concurrent engines, phase-tagged errors, conversions round-trip).
* `src/embed.rs` `#[cfg(test)]` — 6 unit tests (stateful closure counters,
  catchable host errors, `type_of` parity, partial output on failure).
* `cargo run --example embed_hello` — end-to-end host demo.

Known follow-ups (see plan.html R7 residuals): a compiled-bytecode (VM)
embedding path; per-run script args (removing the process-global static);
native closures in the AOT runtime's install path.
