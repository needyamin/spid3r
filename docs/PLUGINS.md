# Plugins

How third-party code extends Spid3r, what it is allowed to do, and what is
actually implemented today.

Status: **Spid3r-source, WASM and host plugins all work**. FFI does not exist
yet. This page marks every route rather than describing a design as if it
shipped.

## The one thing to understand first

A **host plugin is trusted code**. It is compiled into the host binary, runs in
the same address space, and has whatever authority the host has. There is no
sandbox, and none can be added at this layer: a capability declaration stops a
*script* from calling a function, and cannot revoke authority the plugin already
holds.

Only a **WASM plugin** can be genuinely sandboxed, because it has no ambient
authority to begin with. That route is not implemented.

## Three kinds of plugin, two trust stories

| `kind` | How it loads | Trust |
| --- | --- | --- |
| `matrix` | a `.matrix` file the toolchain loads and runs | **script code** — the authority a script has |
| `wasm` | a `.wasm` module the toolchain loads, interpreted by `wasmi` | **sandboxed** — no imports, so no ambient authority |
| `host` | compiled into a Rust host that calls `plugin::register` | **trusted native code** — same address space, same authority |

`matrix` is the default and needs no other language: write `public fn`s, declare
them, done. `wasm` is for code you did not write. `host` is for a Rust host that
wants to expose its own internals.

A host plugin cannot be loaded from disk: it is Rust code, and `Value` has no
stable C ABI to hand it across. The other two *only* exist on disk, which is why
`spidr run app.matrix` can call them without being rebuilt.

## Why a host plugin has no dynamic loading

A native function in Spid3r is:

```rust
type NativeDef = (&'static str, fn(&NativeCtx, Vec<Value>) -> Result<Value, String>);
```

`Value` is a reference-counted enum with no stable representation, so it cannot
cross a C ABI boundary. Loading arbitrary native code at run time would need a
serialisation hop or a handle table, plus `dlopen`. ABI v1 does none of that:
**a plugin is compiled into its host**, like a Cargo build-dependency.

The consequence, stated plainly: the `spidr` binary itself registers no plugins,
so `spidr run` cannot call a plugin function. A host that embeds `spid3r_lang`
calls `plugin::register` at startup, and its scripts can.

## The manifest

```toml
[plugin]
name = "hello"
version = "0.1.0"
abi = "spid3r-plugin-abi-1"      # exact match, or registration fails
kind = "host"                    # host | wasm
capabilities = ["environment"]

[[function]]
name = "hello_greet"
arity = 1                        # no permission: pure

[[function]]
name = "hello_env"
arity = 1
permission = "environment"
```

The manifest is the contract. `register` rejects, **before any plugin code
runs**:

| Rejected | Why |
| --- | --- |
| `abi` is not exactly `spid3r-plugin-abi-1` | a plugin built against another contract is not "probably fine" |
| a function the manifest does not declare | the manifest would be lying about the plugin |
| an arity that disagrees with the manifest | the script and the plugin would disagree about the call |
| a `permission` the `capabilities` list omits | a plugin must not quietly acquire authority it never announced |
| a duplicate plugin/function name, or one that shadows a builtin | `write_file` must not be interceptable |

## The API

```rust
use spid3r_lang::plugin::{PluginFnDef, PluginHost, PluginManifest};
use spid3r_lang::value::{NativeCtx, Value};

pub struct Hello;

impl PluginHost for Hello {
    fn manifest(&self) -> &PluginManifest { /* include_str!("plugin.toml") */ }

    fn functions(&self) -> Vec<PluginFnDef> {
        vec![PluginFnDef {
            name: "hello_greet",
            arity: 1,
            permission: None,
            func: |_ctx, args| match args.first() {
                Some(Value::Str(s)) => Ok(Value::Str(std::sync::Arc::from(
                    format!("Hello, {s}!")))),
                _ => Err("hello_greet(name) expects a string".into()),
            },
        }]
    }

    fn start(&mut self) -> Result<(), String> { Ok(()) }
    fn stop(&mut self) -> Result<(), String> { Ok(()) }
}
```

Register once, at host startup, **before** creating an engine:

```rust
spid3r_lang::plugin::register(Box::new(Hello))?;
```

## Lifecycle

```
Loaded ──start()──▶ Started ──stop()──▶ Stopped
   │                   ▲
   └──start() fails────┴──▶ Failed
```

- `start` runs during `register`, with the registry lock **released**, so a
  plugin may do real work (open a pool, read config) without blocking others.
- A `start` failure is recorded as `Failed` and returns an error to the caller,
  but the plugin is never offered and the host keeps running.
- `stop` runs in `plugin::shutdown_all()`. It must tolerate being called twice;
  `shutdown_all` also skips anything already stopped, and returns errors instead
  of failing fast so one bad teardown cannot leave the rest running.

## The permission gate

A function's `permission` is matched by `Engine::deny`:

```rust
let engine = Engine::new().deny(Capability::Environment);
```

The script then gets a **catchable** `capability denied: 'hello_env' is not
available to this script`, and everything else keeps working. `deny` matches
plugin functions by their declared permission, because `Capability::natives()`
is a fixed list of builtins and cannot contain a plugin's name.

This is the same mechanism as for builtins, with the same caveat: it is a policy
filter over names, not a sandbox.

## Spid3r plugins

The kind that makes the ecosystem belong to the language. A plugin is a
directory with a `plugin.toml` and a `.matrix` file:

```toml
[plugin]
name = "greetings"
abi = "spid3r-plugin-abi-1"
kind = "matrix"
entry = "greetings.matrix"    # optional: defaults to <name>.matrix

[[function]]
name = "greet"
arity = 1
```

```spid3r
public fn greet(name) {
    return "Hello, " + name + "!";
}
```

```console
$ SPID3R_PLUGINS=./plugins spidr run app.matrix
Hello, Ada!
```

### What the loader does

1. Parse and check the file with the ordinary front end — a plugin that does not
   check never loads.
2. Run it as a **module** (`Interp::run_module`): every top-level statement, and
   no `fn main` required. A plugin is definitions, not a program.
3. For each declared function, look the name up in the plugin's globals, verify
   it is a function with the declared arity, and wrap it as a native closure.
4. Register the closures like any other plugin.

### Isolation

Each plugin gets its own interpreter and its own globals, so it cannot read or
clobber the calling program's variables. It *can* call the whole standard library
— that is the point of writing in the language. A `throw` inside the plugin
reaches the caller's `catch` with its message intact.

### Trust

Script authority, tier T1 — the same trust as the project's own source. Being a
plugin does not sandbox it, and this page will not imply that it does. Use
`kind = "wasm"` for untrusted code.

A matrix plugin needs **no** feature flag: it works in the zero-dependency build.

Sample: `examples/plugins/hello-matrix/`.

## WASM plugins

A WASM plugin is a directory with a `plugin.toml` and a `.wasm` module. The
toolchain finds it on the plugin search path and interprets it.

### Discovery

In order: `SPID3R_PLUGINS` (a `;`-separated list of directories that *contain*
plugin directories), `./plugins`, `<exe>/plugins`, `~/.spidr/plugins`. A plugin is
a directory holding `plugin.toml`; the load order is sorted, so a name clash
reports the same plugin every time.

`spidr run`, `check`, `compile`, `exec`, `test`, `bench`, `debug`, `gui` and
`repl` load plugins. **`build` and `dist` do not**, because a standalone native
executable cannot link one — a program that calls a plugin function is rejected
at compile time with an explanation rather than producing a binary that throws
`null is not callable` at run time.

A plugin that fails to load is a **warning**, not a fatal error: one broken
plugin in a shared directory must not stop the toolchain from running a program.

### The guest contract

| export | signature | purpose |
| --- | --- | --- |
| `memory` | — | the linear memory the host reads and writes |
| `alloc` | `(i32) -> i32` | reserve bytes, return the pointer |
| `dealloc` | `(i32, i32) -> ()` | optional; release the request buffer |
| one per function | `(i32, i32) -> i64` | `(ptr, len)` in, `(ptr << 32) \| len` out |

A function's export name is its manifest name, so there is no dispatch table in
the guest.

### The wire format

Little-endian; the full definition is `src/plugin/abi.rs`.

```text
request  := u8 argc, argc × value
response := value | (0xFF, u32 len, utf8 message)

value := u8 tag, payload
  0 null · 1 i64 · 2 f64 · 3 u8 bool · 4 (u32 len, utf8) · 5 (u32 count, values)
```

A typed binary encoding rather than the JSON `plan.txt` sketched, because a guest
would otherwise have to ship a JSON parser *into the sandbox*. A map, a class
instance or a closure is refused with a diagnostic rather than coerced.

### The sandbox, precisely

The host builds an **empty** `Linker`, so a module that imports anything —
`wasi_snapshot_preview1::fd_write`, a clock, randomness — fails to instantiate.
That is the sandbox: the capability is not revoked at run time, it never existed.
The only thing a plugin can touch is the buffer it is handed.

### Fuel

Every call gets a fuel budget (`DEFAULT_FUEL`, 50M instructions, or the host's
tighter limit). A `loop {}` traps instead of hanging the interpreter, and the
trap surfaces as a catchable script error. This is the one place a budget is
enforceable *below* the language, and it is why running an untrusted plugin is
safe.

### Sample

`examples/plugins/hello-wasm/` — a guest crate, its built `plugin.wasm`, and a
demo. `SPID3R_PLUGINS=examples/plugins spid3r run examples/plugins/hello-wasm/demo.matrix`
prints `Hello, Ada! (from wasm)` and `42` on both `tree` and `vm`.

## Writing one

```console
$ cp -r examples/plugins/hello-host my-plugin
$ cd my-plugin && cargo build
```

1. Edit `plugin.toml` — name, version, capabilities, and one `[[function]]` per
   function.
2. Implement `PluginHost`. `manifest()` should `include_str!` your
   `plugin.toml` so the shipped manifest and the code cannot drift.
3. Return one `PluginFnDef` per declared function, with the same arity.
4. Register before creating the engine.

Function names are `&'static str` — compile-time constants. That is what keeps
plugin names out of the marshalling problem: nothing about a plugin's identity is
negotiated at run time.

## What is not implemented

| Route | Status |
| --- | --- |
| Spid3r plugins (`kind = "matrix"`) | **works** — no feature flag, no build step |
| Host plugins (compiled in) | **works** |
| WASM plugins (`kind = "wasm"`, loaded from disk) | **works** — `wasmi`, behind the `plugins-wasm` feature |
| FFI (`extern fn`) | not implemented |
| `spidr plugin list/enable/disable` | not implemented |
| Dynamic loading of *native* code (`dlopen`) | deliberately not implemented — no stable ABI to load against |

`spidr plugin list` is missing for a specific reason: a host plugin belongs to the
host binary that registered it, and a WASM plugin is discovered per run, so there
is no process-independent list to print. A `--json` report of what *would* load is
the useful shape, and it is not written yet.

### Build configurations

`plugins-wasm` is a **default** feature. Without it:

```console
$ cargo build --no-default-features
```

there is no `wasmi` dependency at all, and a `kind = "wasm"` plugin reports
"this build was compiled without the `plugins-wasm` feature" rather than being
silently ignored.
