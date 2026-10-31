# Plugin ABI v1

The contract between the toolchain and a plugin. Version string:

```
spid3r-plugin-abi-1
```

```console
$ spidr plugin abi
spid3r-plugin-abi-1
```

This is the **only** accepted value. A plugin declares it in `plugin.toml` and
the toolchain compares it exactly, at load, before any plugin code runs.

## The three parts of the contract

| Part | Where it lives | Who checks it |
| --- | --- | --- |
| **Manifest** | `plugin.toml` | parsed and validated at load |
| **Function surface** | the declared `[[function]]` list | every declared name must resolve in the plugin |
| **Wire format** | `src/plugin/abi.rs` (WASM only) | the host encodes and decodes every call |

A `matrix` plugin has no wire format: its functions are called directly. A
`host` plugin has no manifest-on-disk: it is compiled in. The manifest and the
function surface are common to all three.

## 1. The manifest

```toml
[plugin]
name = "greetings"                 # unique per process
version = "0.1.0"
abi = "spid3r-plugin-abi-1"        # this document
kind = "matrix"                    # matrix | wasm | host
capabilities = ["environment"]     # everything a function may declare
entry = "greetings.matrix"         # required for wasm; defaults for matrix

[[function]]
name = "greet"
arity = 1
permission = "environment"         # optional
```

Rejected before any plugin code runs:

| Rejected | Because |
| --- | --- |
| a different `abi` | a plugin built against another contract is not "probably fine" |
| a function the plugin does not provide | the manifest would be lying |
| an `arity` that disagrees with the plugin | the manifest and the plugin disagree about the call |
| a `permission` not in `capabilities` | authority must be announced, not acquired |
| a duplicate plugin or function name | two definitions cannot both win |
| a function that shadows a builtin | `write_file` must not be interceptable |
| `kind = "wasm"` through the host path | it would get the host's authority while claiming a sandbox |

## 2. The function surface

Each declared function is callable from a script by its manifest name. What the
name must resolve to depends on the kind:

| kind | must resolve to |
| --- | --- |
| `matrix` | a `public fn` of that name in the `.matrix` file, with the declared arity |
| `wasm` | an export of that name with signature `(i32, i32) -> i64` |
| `host` | a `PluginFnDef` of that name in `PluginHost::functions()` |

## 3. The wire format (WASM)

Little-endian. Defined in `src/plugin/abi.rs`.

```text
request  := u8 argc, argc × value
response := value | (0xFF, u32 byte_len, utf8 message)

value := u8 tag, payload
  0  null      (no payload)
  1  int       i64
  2  float     f64
  3  bool      u8 (0 or 1)
  4  string    u32 byte_len, utf8 bytes
  5  list      u32 count, count × value
```

Rules the host enforces:

* a response must be **fully consumed** — trailing bytes are an error, not slack;
* a claimed length is bounds-checked **before** allocating (8 MiB ceiling);
* a tag outside the table is an error, never a guess;
* `0xFF` is an error value, delivered to the script as a catchable `Err`.

## Versioning policy

**Exact match, no negotiation.** There is no "compatible range", no feature
detection, and no best-effort loading. The toolchain accepts `spid3r-plugin-abi-1`
and nothing else.

That is a deliberate trade: a plugin that half-works is harder to debug than a
plugin that refuses to load, and a silent ABI mismatch produces exactly the kind
of "it compiled but computed something else" failure this project treats as
worst.

### What forces a v2

Any of these is an ABI change, and each requires a new version string:

* a new or changed value tag in the wire format;
* a change to the guest export signatures (`alloc`, `dealloc`, `memory`, the
  per-function `(i32, i32) -> i64` form);
* a new required manifest key, or a changed meaning for an existing one;
* a change to what a declared `arity` means.

Adding an **optional** manifest key that an older plugin can omit does not force
a version bump — the parser must accept its absence.

### Migrating

A v2 would be a second accepted string with its own loader, so both can be
supported at once. v1 is expected to stay loadable for as long as the wire
format does not change, because the cost of a v2 is paid by every plugin author
at once.

## Tooling

```console
$ spidr plugin abi                # the version string this build implements
spid3r-plugin-abi-1

$ spidr plugin list               # what the search path would load
examples/plugins
  hello-matrix (matrix) — 2 function(s)
  hello-wasm (wasm) — 2 function(s)

$ spidr plugin check examples/plugins/hello-wasm
ok: examples/plugins/hello-wasm conforms to spid3r-plugin-abi-1
  kind wasm · 2 function(s) · capabilities []
  2 export(s) resolved · 65745 byte(s) of module
```

`check` runs the **same validation the loader does**, without registering
anything, so a passing check and a failing load cannot disagree. Exit codes:
`0` valid, `1` invalid, `2` usage.

A `kind = "host"` directory cannot be validated from disk — it is Rust compiled
into a host — and `check` says so rather than passing it.

## What the toolchain guarantees

* A plugin that loads has passed every check above.
* A plugin that fails to load never offered a function; it cannot shadow or
  half-register.
* A plugin's functions behave identically on every engine that installs from
  `stdlib::native_defs()` — the tree interpreter and the bytecode VM.
* A standalone AOT executable cannot carry a plugin, and says so at build time.

## What it does not guarantee

* **Sandboxing.** Only `kind = "wasm"` is sandboxed, and only because the host
  links no imports into it. A `matrix` or `host` plugin is trusted code.
* **Stability of the plugin's own behaviour.** The ABI fixes the boundary, not
  what the plugin does behind it.
* **Thread safety.** A plugin call is serialized by the evaluator; a plugin that
  spawns its own threads is on its own.
