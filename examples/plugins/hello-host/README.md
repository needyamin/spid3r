# hello-host — a sample Spid3r host plugin

A complete, working plugin. It is small on purpose: two functions, one of them
permission-gated, and a host binary that loads it.

```console
$ cargo build
$ SPID3R_DEMO_VAR=demo-var ./target/debug/hello-host demo.matrix
[hello] started
Hello, Ada!
demo-var
[hello] stopped

$ SPID3R_DEMO_VAR=demo-var ./target/debug/hello-host deny.matrix --deny-environment
[hello] started
caught: capability denied: 'hello_env' is not available to this script
Hello, still works!
[hello] stopped
```

## The files

| File | What it is |
| --- | --- |
| `plugin.toml` | The manifest — the contract. Names, ABI string, and every function with its arity and permission. |
| `src/lib.rs` | The plugin: `PluginHost` with `manifest()`, `functions()`, `start()`, `stop()`. |
| `src/main.rs` | The host: registers the plugin, then runs a script. |
| `demo.matrix` | Calls both functions. |
| `deny.matrix` | Shows the permission gate: the denied call is catchable. |

## How it works

A host plugin is **compiled into its host**. There is no dynamic loading in ABI
v1: a `NativeDef` is a Rust function pointer over `Value`, and `Value` has no
stable representation that could cross a C ABI boundary. So the plugin depends
on the toolchain, not the other way round.

```rust
spid3r_lang::plugin::register(Box::new(HelloPlugin::new()))?;
```

Registration merges the plugin's functions into `stdlib::native_defs()`, the one
list the tree interpreter, the bytecode VM and the AOT backend all install from.
A plugin function therefore needs **no per-engine code**.

`register` rejects, before any plugin code runs:

- an `abi` that is not exactly `spid3r-plugin-abi-1`;
- a function the manifest does not declare, or one whose arity disagrees;
- a function whose `permission` the manifest's `capabilities` does not announce;
- a duplicate plugin or function name, or a function that would shadow a builtin.

A plugin whose `start()` fails is recorded as `Failed` and simply never offers
its functions — one broken plugin cannot take the host down.

## The permission gate, honestly

`hello_env` declares `permission = "environment"`. A host that calls
`Engine::deny(Capability::Environment)` replaces it with a stub returning a
catchable `capability denied` error, which is what `deny.matrix` demonstrates.

**This is a policy filter, not a sandbox.** The plugin is trusted code: it runs
in the host's address space and could call `std::env::var` whether or not it
declared anything. The declaration is what lets a host stop a *script* from
reaching it. It does not constrain the plugin, and nothing here can revoke
authority the plugin already has. Only a WASM plugin — which has no ambient
authority to begin with — can be genuinely sandboxed, and that is not
implemented yet.

## Writing your own

1. Copy this directory.
2. Change `name`/`version` in `plugin.toml` and the functions you declare.
3. Implement `PluginHost` — `manifest()` returns your parsed `plugin.toml`
   (`include_str!` it, as this sample does), `functions()` returns one
   `PluginFnDef` per declared function.
4. Register it at host startup, before creating an `Engine`.

Function names are `&'static str`, so they must be compile-time constants. That
is not an arbitrary restriction — it is what keeps plugin names out of the
marshalling problem entirely.
