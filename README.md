<pre>
███████╗██████╗ ██╗██████╗ ██████╗ ██████╗         ╱     ╲
██╔════╝██╔══██╗██║██╔══██╗╚════██╗██╔══██╗    ╱╲  ╲ ▄█▄ ╱  ╱╲
███████╗██████╔╝██║██║  ██║ █████╔╝██████╔╝   ╱  ╲──╲▀█▀╱──╱  ╲
╚════██║██╔═══╝ ██║██║  ██║ ╚═══██╗██╔══██╗  ╱  ╱──╱▄███▄╲──╲  ╲
███████║██║     ██║██████╔╝██████╔╝██║  ██║    ╱  ╱ █████ ╲  ╲
╚══════╝╚═╝     ╚═╝╚═════╝ ╚═════╝ ╚═╝  ╚═╝    ╲  ╲ ▀███▀ ╱  ╱
</pre>

**⚡ One Frontend. Four Backends. Zero Runtime Dependencies.**

Spid3r is a general-purpose programming language (`.matrix` / `.mt`) designed for high performance, reliability, and portability. Its interpreters and compilers deliver consistent behavior across all four backends—including identical standard output, error phases, and exit codes.

<p align="center">
  <img width="800" alt="Spid3r Introduction" src="https://github.com/user-attachments/assets/cc61998d-95f0-408f-942f-ec4b1256c4ac" />
</p>

<p align="center">
  <a href="https://spid3r-spkg.github.io/"><strong>📦 Package Registry</strong></a> ·
  <a href="docs/html/doc.html">📚 Documentation</a> ·
  <a href="docs/html/package.html">📦 Package Guide</a> ·
  <a href="docs/html/playground.html">💻 Playground</a> ·
  <a href="docs/html/benchmark.html">📊 Benchmarks</a>
</p>

<p align="center">
  <a href="https://apps.microsoft.com/detail/9NPH5XX0F6KF?hl=en&gl=BD&ocid=pdpshare">🪟 Microsoft Store</a> ·
  <a href="https://marketplace.visualstudio.com/items?itemName=ANSNEWTECH.spid3r-formatter">🧩 VS Code Extension</a> ·
  <a href="https://github.com/needyamin/spid3r/releases">🚀 Releases</a>
</p>

## Spid3r Pipeline

![Spid3r pipeline: one front end, four backends](docs/diagrams/pipeline.svg)

```js
fn main() {
    println("Hello, Spid3r!");
    return 0;
}
```

```bash
$ spid3r run hello.matrix
  Hello, Spid3r!

$ sp run hello.mt
  Hello, Spid3r!
```

## Install

```bash
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/needyamin/spid3r/main/install.sh | sh

# From source (core VM / interpreter, zero external dependencies)
cargo install --path . --no-default-features
```

- **Windows:** install directly from the **[Microsoft Store](https://apps.microsoft.com/detail/9NPH5XX0F6KF?hl=en&gl=BD&ocid=pdpshare)** (`9NPH5XX0F6KF`, instant `AppExecutionAlias` commands on `PATH`) or run `Spid3r-Setup-<version>.exe` from the [latest release](https://github.com/needyamin/spid3r/releases) (bundles `spid3r-self`, `spid3r`, `sp`, `yamc`, `yamcvm`, `spvm`, `spkg`, `spid3r-control`, LLVM, `.matrix`/`.mt` file associations, and configures `PATH` for both the toolchain and `spkg tool` shims).
- **Core CLI (`tree` / `vm` / `repl`):** zero runtime dependencies.
- **Desktop GUI (`spid3r gui`):** uses OS WebView (WebView2 on Windows 10/11, WebKit2GTK + GTK3 on Linux).
- **Native AOT (`spid3r build` / `spid3r-self`):** LLVM 23.1+; outputs standalone native executables.
- **VS Code Extension:** install [Spid3r Formatter (`ANSNEWTECH.spid3r-formatter`)](https://marketplace.visualstudio.com/items?itemName=ANSNEWTECH.spid3r-formatter) from the Visual Studio Marketplace for `.matrix`/`.mt` syntax highlighting and formatting (also bundled offline in the installers).

## The language

```js
interface Greetable {
    public fn greet(self) -> string { return "Hi, " + self.name; }
}

class User implements Greetable {
    public let name: string;
    fn new(name: string) { self.name = name; }
    public static fn guest() { return new User("Guest"); }
}

fn main() {
    let u = User("Ada");
    let scores = { "math": 95, "cs": 100 };
    let doubled = [1, 2, 3].map(x => x * 2);
    println(u.greet() + " / cs=" + string(scores["cs"]) + " / " + string(doubled));
    return 0;
}
```

Nullable types (`T?`, `int | string`) with flow-sensitive narrowing (`if x != null`, `?`, `?.`), classes, interfaces, enums with exhaustive `match`, generics, closures, structured `try`/`catch`, `spawn` + channels, `20+` built-in containers, vectors/matrices, `26` embedded stdlib modules (`254` native functions), and multi-error `E1xxx` diagnostics.

## Architecture & backends

Source passes through a single checked front end (`lexer` → `parser` → `semantic` → `lower`) consumed by four backends verified together by `spid3r test`:

- **`tree`** — AST interpreter and differential semantic reference oracle.
- **`vm`** — 60-opcode bytecode VM (**`spvm`**) running `.matrixc` compiled by **`yamcvm`**.
- **`native`** — LLVM 23 ahead-of-time native compiler (**`yamc`** / **`spid3r build`** and self-hosted **`spid3r-self`**) backed by **YamGC** (deterministic reference counting + cycle collector).
- **`wasm`** — `wasm32` bytecode target for browser execution (`spid3r bundle --target wasm`).

**Self-hosting:** Stage 1/2/3 self-hosted compiler (`spid3r-self` in `selfhost/`) drives `tokens`, `parse`, `check`, `compile`, `build`, `test`, `fmt`, and package artifact checks against the SHA-256-pinned `spid3r_lang` runtime — see [docs/selfhost/](docs/selfhost/README.md).

## Packages & CLI tools (`spkg`)

Spid3r ships an in-process package manager (**`spkg`**) connected out of the box to the official public registry at **[https://spid3r-spkg.github.io/](https://spid3r-spkg.github.io/)**.

```bash
# Install a library into vendor/ and lock with SHA-256 + Ed25519
spkg init my_app && cd my_app
spkg install nettools
spkg verify && spkg audit

# Enforce lockfile integrity strictly in CI (or air-gapped caches)
spkg install --frozen --offline

# Install & run verified global CLI tools declaring [bin]
spkg tool install nettools
nettools subnet 192.168.1.0/24

# Publish signed source releases or target wheels to a static registry
SPID3R_SIGN_KEY=<64-hex-seed> spkg publish --to-dir ./registry
```

![Four routes by which third-party code reaches a Spid3r program](docs/diagrams/ecosystem.svg)

| Route | Trust & Integrity | Status |
|---|---|---|
| Embedded stdlib | Immutable, compiled in (`26` modules, `254` natives) | Shipped |
| Project `.matrix` / `.mt` | Local workspace source | Shipped |
| Vendored packages & wheels | Zero install scripts · SHA-256 lock + fail-closed Ed25519 (`spkg verify` / `audit` / `--frozen`) | Shipped |
| Native C FFI (`extern fn`) | Trusted C ABI + verified vendored `[ffi]` auto-linking (AOT only) | Shipped |

## Toolchain & component CLIs

All commands below accept both `.matrix` and `.mt` sources and are available via `spid3r` (short alias `sp`) as well as focused single-role binaries:

| Binary | Role | Key Commands |
|---|---|---|
| **`spid3r`** / **`sp`** | Unified CLI | `run`, `check`, `compile`, `build`, `exec`, `disasm`, `test`, `bench`, `fmt`, `doc`, `lsp`, `repl`, `debug`, `gui`, `bundle`, `info`, `config`, `control` |
| **`spkg`** | Package manager | `init`, `add`, `remove`/`uninstall`, `install`, `list`, `show`, `outdated`, `audit`, `freeze`, `tree`, `verify`, `search`, `publish`, `pack`, `unpack`, `cache`, `tool`, `dist`, `modules` |
| **`yamc`** | Native AOT compiler | `yamc app.matrix -o app --release`, `build`, `check`, `compile`, `verify-runtime` |
| **`yamcvm`** | Bytecode compiler | `yamcvm app.matrix -o app.matrixc`, `compile`, `disasm`, `check`, `dist` |
| **`spvm`** | Bytecode VM | `spvm app.matrixc`, `exec`, `run`, `disasm`, `repl`, `debug` |

**Exit codes:** `0` ok · `1` compile, runtime, or broken `SPID3R_INI` config pin · `2` CLI usage error · `4` corrupt or unpinned `.matrixc` / runtime staticlib.

## Documentation

| Guide | Covers |
|---|---|
| [Public Package Registry (`spkg`)](https://spid3r-spkg.github.io/) | Browse packages, versions, SHA-256 digests, and verified Ed25519 status |
| [Language Guide](docs/html/doc.html) · [Package Guide (`spkg`)](docs/html/package.html) | Tutorial, stdlib API reference, authoring, lockfiles, workspaces, and signing |
| [Playground](docs/html/playground.html) · [Benchmarks](docs/html/benchmark.html) | Browser WASM playground and VM/tree/native performance results |
| [Grammar](docs/GRAMMAR.md) · [Standard Library](docs/STANDARD_LIBRARY.md) | EBNF, precedence, modules, and native functions |
| [Data Structures](docs/DATA_STRUCTURES.md) · [Garbage Collection](docs/GC.md) | Containers, vectors/matrices, RC + cycle collector |
| [Native Backend](docs/NATIVE.md) · [FFI](docs/FFI.md) · [Runtime ABI](docs/selfhost/RUNTIME_ABI.md) | LLVM AOT codegen, `extern fn` linking, SHA-256 runtime contract |
| [Self-Hosting Docs](docs/selfhost/README.md) · [ADRs](docs/selfhost/SELFHOST.md) | Bootstrap stages, compiler pipeline, and ownership boundaries |
| [GUI](docs/GUI.md) · [Embedding](docs/EMBEDDING.md) | Desktop WebView apps and Rust host embedding |
| [Environment](docs/ENVIRONMENT.md) · [Security Policy](SECURITY.md) | `SPID3R_*` variables, capability filters, trust model & reporting |

## License

GNU General Public License v3.0 or later with the **Spid3r Runtime Library Exception** (GPLv3 Section 7) — see [LICENSE.md](LICENSE.md). Compiled `.matrixc` bytecode, native executables, and `bundle` outputs may be distributed under any license of your choice.
