# The Spid3r ecosystem

How third-party code reaches a Spid3r program, what each route is allowed to do,
and which parts exist today.

Status: every route on this page is implemented. `spidr-<name>` external tools,
packages, Spid3r-source / WASM / host plugins, and native FFI.

## 0. The seven routes, at a glance

![Seven routes by which third-party code reaches a Spid3r program](diagrams/ecosystem.svg)

Solid rows ship today; dashed rows are planned (P4–P6). Only the WASM route can
be sandboxed.

## 1. The seven ways code enters a program

| Route | Today | Trust | Notes |
| --- | --- | --- | --- |
| Embedded stdlib | ✅ | T0 | Compiled into the binary; immutable, no disk override from the install tree |
| Project `.matrix` files | ✅ | T1 | Same trust as the project's own source |
| Spid3r plugins | ✅ | T1 | `kind = "matrix"`: a `.matrix` file whose `public fn`s are exposed. Script trust |
| Vendored packages (`vendor/`, `spider.lock`) | ✅ | T2 | Checksummed on install |
| Host plugins | ✅ | T4 | **Trusted code.** Not sandboxed, and cannot be — see `docs/PLUGINS.md` |
| WASM plugins | ✅ | T3 | The only genuine sandbox: empty linker, so no ambient authority; fuel-limited |
| FFI (`extern fn`) | ✅ | T5 | **Trusted code**, AOT-only — see `docs/FFI.md` |

The trust tiers are defined in `docs/SECURITY.md`. The one rule that matters
most: **capability filtering is a policy filter, not a sandbox.** It replaces
known builtin natives with stubs that return a catchable `capability denied`
error. A host function a plugin registers keeps whatever authority it was given,
and no filter can revoke it.

## 2. `spidr-<name>` external tools

Any executable named `spidr-<name>` on `PATH` is a Spid3r subcommand:

```console
$ spidr fmt-ext main.matrix     # runs spidr-fmt-ext with ["main.matrix"]
$ echo $?
0
```

Rules, all enforced in `src/cli.rs`:

* **Built-ins always win.** `spidr version` never looks for `spidr-version`.
* **The exit code is the tool's.** `0`, `1` and `2` keep their documented
  meanings (success, user-program error, usage error); any other code is passed
  through unchanged; death by signal maps to `1`.
* **Arguments are forwarded verbatim**, after the subcommand name.
* **`--help` reaches the tool.** `spidr fmt-ext --help` runs `spidr-fmt-ext`
  with `--help`; the tool's own help is what the user sees. An unknown command
  whose tool does not exist still gets Spid3r's help appended to the usage
  error, so the output never leaves the user without a next step.
* **The name is validated before any lookup.** The executable path is built by
  concatenation (`spidr-` + name), so a name starting with `-` or `.`,
  containing a path separator, a drive/UNC prefix, or a `..` component is
  rejected with the ordinary unknown-command usage error.
  `spidr ../../bin/sh` runs nothing.
* **On Windows, only `.exe` and `.com` are dispatched.** Batch shims
  (`.bat`/`.cmd`) are excluded on purpose: `cmd.exe` re-parses the command line
  it is handed, so a single argument containing `&` or `|` becomes a second
  command. A tool invoked as `spidr lint "$PR_FILE"` would then execute whatever
  a pull request put in the filename. Git for Windows refuses to dispatch to
  batch files for the same reason. Ship a real executable instead.
* **`PATH` is honoured per platform.** Entries are split with the platform
  separator; empty entries are skipped; a *relative* entry such as `.` is
  honoured as given, exactly as the shell does.

This is the cheapest extension point in the ecosystem: no ABI, no manifest, no
version negotiation. It is also the only one that needs no cooperation from the
language — which is why it shipped first.

### Why not JSON-RPC yet

Out-of-process plugins over newline-delimited JSON-RPC is the C-ABI-free way to
extend Spid3r without WASM (`src/rpc/`, `spidr rpc`). It is deliberately
**deferred until the plugin ABI in `src/plugin/` exists**: the method set, the
capability model and the error envelope all belong to that contract, and
inventing them here would produce a second, incompatible plugin protocol. The
external-tool dispatch above already covers "run someone else's binary".

## 3. Writing an external tool

A tool is a normal executable. It receives the arguments the user typed after
the subcommand name, and whatever it prints is what the user sees.

```sh
#!/bin/sh
# spidr-fmt-ext — an external formatter for .matrix files.
set -eu
for f in "$@"; do
  [ -f "$f" ] || { echo "no such file: $f" >&2; exit 2; }
done
echo "formatted $# file(s)"
```

Two conventions worth following, because they are what the built-ins do:

* **Exit 2 for usage errors, 1 for "the user's input is wrong", 0 for success.**
  A tool that exits 0 after failing silently makes the whole command
  untrustworthy.
* **Write diagnostics to stderr**, never stdout — stdout belongs to the program
  being processed, so `spidr foo | head` keeps working.

## 4. Packages

`spidr install` vendors path and git dependencies into `vendor/` and records a
pinned commit plus a SHA-256 per dependency in `spider.lock`; `vendor/` is on the
module search path (see `docs/REGISTRY.md` and `docs/BUILDING.md` → "Module
resolution"). Dependency installation **never executes package code** — there are
no build scripts, unlike Cargo. That is a real security advantage and it is
intentional; do not add them.

## 4a. Plugins

A **host plugin** is a Rust type implementing `PluginHost`, declared in a
`plugin.toml`, and registered by its host before any engine exists:

```rust
spid3r_lang::plugin::register(Box::new(HelloPlugin::new()))?;
```

Registration merges its functions into `stdlib::native_defs()`, the one list all
three engines install from — so a plugin function needs no per-engine code. A
plugin is **compiled into** its host; there is no `dlopen`, because a native
function is a Rust pointer over `Value` and `Value` has no stable C
representation.

`register` rejects, before any plugin code runs: a wrong `abi`, a function the
manifest never declared or one whose arity disagrees, a `permission` the
manifest does not announce, a duplicate name, and a function that would shadow a
builtin. A `kind = "wasm"` plugin is refused outright rather than loaded through
the host path, where it would silently get the host's full authority.

A complete, runnable sample: `examples/plugins/hello-host/`.

A **WASM plugin** is the other half: it lives on disk, so `spidr run` can use it
without being rebuilt. Discovery is `SPID3R_PLUGINS`, `./plugins`,
`<exe>/plugins`, `~/.spidr/plugins`; the guest exports `memory`, `alloc`, and one
`(i32, i32) -> i64` function per manifest entry. The host links nothing into it,
so the sandbox is structural rather than a filter, and a fuel budget stops a
runaway guest. `spidr build` refuses a program that calls one, because a
standalone binary cannot carry it.

Sample: `examples/plugins/hello-wasm/`. The contract in full: `docs/PLUGINS.md`.

## 5. Resolution and the module search path

Every backend resolves `import "name"` through one implementation,
`src/modules/resolve.rs`, in this fixed order: the importing file's directory,
`SPID3R_PATH`, `./stdlib`, the shipped `stdlib/`, `vendor/`, then the embedded
stdlib table. Disk always beats the embedded copy, which is what lets a project
pin a patched stdlib module. Details and the spec-validation rules are in
`docs/BUILDING.md` → "Module resolution".
