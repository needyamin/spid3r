# hello-wasm — a sample Spid3r WASM plugin

The sandboxed counterpart to `hello-host`. The toolchain interprets this module
and the module can do nothing except read the bytes it is given and write bytes
back: no filesystem, no process, no network, no clock, no randomness. Those
capabilities are not revoked at run time — the host links no imports, so they
never existed.

```console
$ SPID3R_PLUGINS=examples/plugins spid3r run examples/plugins/hello-wasm/demo.matrix
Hello, Ada! (from wasm)
42
```

That works on `--engine tree` and `--engine vm` with identical output, and no
plugin was compiled into the `spid3r` binary: it was discovered from disk.

## Files

| File | What it is |
| --- | --- |
| `plugin.toml` | `kind = "wasm"`, `entry = "plugin.wasm"`, one `[[function]]` per export |
| `src/lib.rs` | the guest: `alloc`, `dealloc`, and one export per function |
| `plugin.wasm` | the built module (committed, so the sample runs without a wasm toolchain) |
| `demo.matrix` | calls `hello_greet` and `hello_sum` |

## The guest contract

A module must export:

| export | signature | purpose |
| --- | --- | --- |
| `memory` | — | linear memory the host reads and writes |
| `alloc` | `(i32) -> i32` | reserve `len` bytes, return the pointer |
| `dealloc` | `(i32, i32) -> ()` | optional; release the request buffer |
| one per function | `(i32, i32) -> i64` | take `(ptr, len)`, return `(ptr << 32) \| len` |

A function's export name is its manifest name, so there is no dispatch table
inside the guest — the host resolves the name once, at load.

## The wire format

Little-endian, defined in `src/plugin/abi.rs` of the toolchain:

```text
request  := u8 argc, argc × value
response := value | (0xFF, u32 len, utf8 message)

value := u8 tag, payload
  0 null · 1 i64 · 2 f64 · 3 u8 bool · 4 (u32 len, utf8) · 5 (u32 count, values)
```

A typed binary encoding rather than JSON, so the guest does not have to ship a
JSON parser *into the sandbox*. Anything outside those six types — a map, a
class instance, a closure — is refused with a diagnostic instead of being
coerced.

## Build it

```console
$ rustup target add wasm32-unknown-unknown
$ cargo build --target wasm32-unknown-unknown --release
$ cp target/wasm32-unknown-unknown/release/spid3r_plugin_hello_wasm.wasm plugin.wasm
```

Commit `plugin.wasm` alongside the source: the sample is meant to run on a
machine that has no wasm toolchain, and `tests/plugin_wasm.rs` fails if the
committed module is stale or not a WebAssembly binary.

## Writing your own

1. Copy this directory, change `name` and the `[[function]]` blocks.
2. Export one `(i32, i32) -> i64` function per entry, plus `alloc`/`dealloc`.
3. Keep `memory` exported — a cdylib does this by default.
4. Build, copy the `.wasm` next to `plugin.toml`, and put the plugin directory
   on `SPID3R_PLUGINS`, in `./plugins/`, beside the executable, or in
   `~/.spidr/plugins/`.

## What it cannot do

A native executable (`spidr build`) cannot link a plugin loaded from disk, so a
program that calls one is **rejected at build time** with an explanation. Run it
with `spidr run`, or ship a host binary that registers the plugin itself.
