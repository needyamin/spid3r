# Building Spid3r from source

Spid3r is a single Rust crate whose **core has no third-party
dependencies**. Two features add heavyweight dependencies, and both are on by
default:

| Feature | Adds | Requires |
| --- | --- | --- |
| *(core, `--no-default-features`)* | — | Rust toolchain only |
| `gui` *(default)* | embedded webview GUI (`spid3r gui`) via tao + wry | system webview runtime (see below) |
| `native` *(default)* | LLVM AOT backend (`spid3r build`) via inkwell 0.10 | LLVM 18.1+ |

Without LLVM installed, build with `--no-default-features --features gui`.

Everything below assumes the repository root as the working directory.

## Prerequisites

- **Rust** — **1.99.1**, pinned in `rust-toolchain.toml` (rustup installs it
  automatically on the first `cargo` invocation in this directory). The version
  is pinned because CI lints with `-D warnings`: a new stable can deprecate an
  API and turn the whole matrix red overnight (Rust 1.99 renamed
  `AtomicUsize::fetch_update` to `try_update`, which is exactly what happened).
  To upgrade: bump `rust-toolchain.toml` **and** the `toolchain:` inputs in
  `.github/workflows/ci.yml`, then run the gates. Edition 2021; `rust-version`
  in `Cargo.toml` records the MSRV.
- **A C toolchain / linker** — only needed for the `native` feature:
  - Windows: MSVC (`link.exe`) from Visual Studio Build Tools **and** the
    Windows 10/11 SDK.
  - macOS: Xcode command line tools (`clang`).
  - Linux: `cc` / `clang` plus `libc` dev files.
- **LLVM 18.1+** — only needed for the `native` feature.
- **A system webview runtime** — only needed for the `gui` feature:
  - Windows: the evergreen **WebView2 Runtime** (preinstalled on Windows 11 and
    on patched Windows 10; otherwise install from Microsoft).
  - macOS: the system **WKWebView** — always present.
  - Linux: **WebKit2GTK** (`libwebkit2gtk-4.1-dev` on Debian/Ubuntu) plus
    `libgtk-3-dev`, `libxdo-dev` (for `xdotool`-style synthetic input) and
    `libssl-dev`.

## Installing LLVM 18.1+

**Windows** — run the official `LLVM-18.1.x-win64.exe` installer from the
LLVM GitHub releases. The default location `C:\Program Files\LLVM` is probed
automatically; for a custom location set `SPID3R_LLVM_LIB_DIR` (see below).

**macOS** — `brew install llvm@18`.

**Linux (Debian/Ubuntu)** — `sudo apt install llvm-18 libllvm18 llvm-18-dev`
(distros shipping `llvm-18` expose `llvm-config-18`, which `build.rs` probes
alongside plain `llvm-config`).

If LLVM is missing, the build still **succeeds** with a
`cargo:warning=LLVM 18 not found ...` and `spid3r build` later fails with a
usage error pointing at this document — the rest of the toolchain
(`run`, `check`, `compile`, `exec`, `repl`, `test`) works fine.

## Build

```
# default: core toolchain + embedded webview GUI + LLVM native backend
# (on Windows this also writes Spid3r-Setup-<version>.exe; see "Windows installer")
cargo build --release

# without LLVM: core toolchain + GUI
cargo build --release --no-default-features --features gui

# zero-dependency core toolchain only (tree interpreter + bytecode VM)
cargo build --release --no-default-features

# run without installing
cargo run --release -- build examples/hello.matrix -o hello
./hello
```

The `spid3r` binary is at `target/release/spid3r` (`.exe` on Windows).

The default `gui` feature pulls in `tao` (window) and `wry` (webview), so
first-time compilation takes noticeably longer than the core toolchain; use
`--no-default-features` to skip those crates.

### Windows installer

On Windows, `cargo build --release` also writes `Spid3r-Setup-<version>.exe`
(version from `Cargo.toml`) to the repository root. A build script runs before
its own crate is compiled, so `build.rs` first builds a release `spid3r.exe`
with the same features into `target/installer/`, then runs NSIS on
`packaging/nsis/spid3r.nsi`. Release builds therefore compile `spid3r` twice
when sources change.

- NSIS 3.x is found in `packaging/nsis/nsis/nsis-3.10/` (the portable zip) or
  on `PATH`; when it is missing, the step is skipped with a `cargo:warning`.
- The installer packs the binary plus `README.md`, `docs/`, `examples/`, and
  (with `gui`) the `calculator/` demo, straight from the repository.
- With `native`, it also ships `LLVM-C.dll` (loaded by `spid3r.exe`),
  `LLVM-C.lib`, and the `spid3r_lang.lib` runtime staticlib, which `spid3r build`
  links into every program. Users still need MSVC Build Tools with the
  Windows SDK to link; the programs they build have no LLVM dependency.
- Set `SPID3R_NO_INSTALLER=1` to skip the installer step.

#### WebView2 bootstrapper (opt-in, verified)

The installer can bundle Microsoft's evergreen WebView2 bootstrapper so a machine
without the runtime can install it. That download is **opt-in**: a plain
`cargo build --release` performs no network access.

```bash
# 1. Fetch and bundle it (needs the hash of the file you verified).
SPID3R_FETCH_WEBVIEW2=1 cargo build --release

# 2. If Microsoft rotated the bootstrapper, pin the new hash explicitly.
SPID3R_FETCH_WEBVIEW2=1 SPID3R_WEBVIEW2_SHA256=<64-hex> cargo build --release
```

The file is downloaded to `packaging/nsis/redist/*.part`, verified against the
pinned SHA-256 (`build_support/webview2.rs`) **and** a minimum size, and only then
renamed into place. A partial or tampered download is deleted and the installer is
built without the bootstrapper (`!ifdef WEBVIEW2_SETUP` in `spid3r.nsi`) — it is
never bundled unverified.

GUI programs can be run headlessly with `spid3r gui --dry-run <file>`, which
replays the whole native↔JS bridge (script flushes and event dispatch) without
opening a window — useful for CI and for verifying handler wiring. See
[GUI.md](GUI.md).

### Environment variables

| Variable | Meaning |
| --- | --- |
| `SPID3R_NO_INSTALLER` | Any value skips the Windows installer step of `cargo build --release`. |
| `SPID3R_LLVM_LIB_DIR` | Directory containing `LLVM-C.lib` (Windows) or `libLLVM-18.so` / `libLLVM.dylib` (Unix). Checked first by both `build.rs` and the linker stage. |
| `SPID3R_LLVM_BIN_DIR` | Windows: directory containing `LLVM-C.dll` (default: `C:\Program Files\LLVM\bin`). `build.rs` copies it next to the built binaries (and into `deps/` for test executables) so `cargo run` / `cargo test` work without modifying PATH. |
| `LLVM_SYS_181_PREFIX` | The llvm-sys convention prefix; `<prefix>/lib` is searched as a fallback when the variables above are unset. |

Set them when LLVM lives somewhere non-standard, e.g.:

```
SPID3R_LLVM_LIB_DIR=/opt/llvm-18/lib SPID3R_LLVM_BIN_DIR=/opt/llvm-18/bin \
  cargo build --release --features native
```

## How the LLVM link works

`inkwell`/`llvm-sys` are built with `no-llvm-linking` because llvm-sys has no
`llvm-config` story on the stock Windows installer and no dynamic-linking
support there. Instead, `build.rs` links the LLVM C API itself:

- **Windows**: locates `LLVM-C.lib` (import library for `LLVM-C.dll`) via
  `SPID3R_LLVM_LIB_DIR` → `LLVM_SYS_181_PREFIX\lib` →
  `C:\Program Files\LLVM\lib`.
- **Linux/macOS**: runs `llvm-config --libdir` (Debian emits `-lLLVM-18`,
  Homebrew `-lLLVM`), with Homebrew prefix fallbacks.

When *linking a compiled program*, `link.rs` additionally needs a system
linker — MSVC `link.exe` plus the CRT/SDK import libraries on Windows,
`clang`/`cc` elsewhere — and the runtime staticlib produced by the crate's
`[lib] crate-type = ["rlib", "staticlib", "cdylib"]` (`libspid3r_lang.a` /
`spid3r_lang.lib`, found next to the `spid3r` binary or under
`target/{release,debug}/`). See
[NATIVE.md](NATIVE.md) for what the linker passes and why (notably
`-opt:ref`, which dead-strips the inkwell bindings so the shipped executable
has **no** `LLVM-C.dll` dependency — verified by running built binaries from
an isolated directory).

## Building the VS Code extension

The language server is part of the compiler, so building the compiler is all the
server needs:

```
cargo build --no-default-features
```

The editor client lives in `packaging/vscode/` and has **no dependencies and no
build step** — `package.json` plus one `extension.js`. Package it into a `.vsix`
with the official packager:

```
cd packaging/vscode
npx --yes @vscode/vsce package      # writes spid3r-0.1.0.vsix
code --install-extension spid3r-0.1.0.vsix
```

`npx` fetches `@vscode/vsce` on demand, so nothing has to be installed
permanently and no `npm install` is required in that directory. To develop
against it, open `packaging/vscode` in VS Code and press `F5` to launch an
Extension Development Host.

If `spid3r` is not on your `PATH`, set `spid3r.serverPath` to its absolute path
in VS Code settings. Set `spid3r.trace.server` to `messages` or `verbose` to
log the JSON-RPC traffic to the *Spid3r Language Server* output channel — that
channel is also where any server startup failure is reported. See
[LSP.md](LSP.md) for the protocol surface.

## Tests and verification

```
cargo test                       # full suite: core, GUI bridge + dry-run, native runtime + differential
cargo test --no-default-features # core engines only
cargo run -- test                # tree == vm == exec over every examples/*.matrix
bash tools/native_diff.sh examples/*.matrix   # tree vs native differential
```

Feature configurations are tested **sequentially**, never concurrently: they all
write to the same `target/debug/spid3r`, and a concurrent build with a different
feature set silently overwrites the binary another test run is using.

`tools/native_diff.sh` wraps every step in a timeout and prints `HANG <name>`
if one ever stalls, so a wedged build or binary can never silently eat the
whole run; tune with `VM_TIMEOUT` / `BUILD_TIMEOUT` / `RUN_TIMEOUT`.

Code quality gate (project convention):
`cargo clippy --all-targets -- -D warnings` — keep `src/` at **0** warnings
(warnings as well as errors; plain clippy lets lints like `map_identity` slip
through as warnings). `bash tools/gate.sh` runs this for every feature
configuration plus a release build.

## Troubleshooting

| Symptom | Cause / fix |
| --- | --- |
| `cargo:warning=LLVM 18 not found for the native backend` | Install LLVM 18.1+ or set `SPID3R_LLVM_LIB_DIR`. Harmless if you don't need `spid3r build`. |
| `no MSVC linker (link.exe) found` (on `spid3r build`) | Install Visual Studio Build Tools, or run from a Developer prompt (`VCINSTALLDIR` is honored). |
| `no Windows SDK found` | Install the Windows 10/11 SDK component. |
| `cannot find the Spid3r runtime library` | Build the toolchain first: `cargo build --features native` produces the staticlib that program links use. |
| `linking failed` with `LNK2019` on `LLVM*` symbols | The final link needs `LLVM-C.lib` on the libpath; a non-default LLVM location needs `SPID3R_LLVM_LIB_DIR` set for the linker stage too. |
| `error deleting lock file ... (os error 5)` during builds | A OneDrive-over-Windows incremental-build quirk, not a code issue; vanishes after `cargo clean`. |
| `spid3r build` says the backend is not enabled | The binary was built with `--no-default-features` (and without `native`); rebuild with the defaults. |
| `spid3r gui` says the GUI is not enabled | The binary was built with `--no-default-features`; rebuild without it (or add `--features gui`). |
| `spid3r gui` builds but no window appears / a create-error is printed | The system webview runtime is missing or too old: WebView2 on Windows, WebKit2GTK on Linux (`libwebkit2gtk-4.1-dev`). `--dry-run` still works without it. |
