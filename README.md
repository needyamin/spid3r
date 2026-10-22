# Spid3r

A fast, lightweight programming language with optional static types, 20+ built-in data structures, dual execution engines, and **zero external dependencies** for the core toolchain.

```text
// hello.matrix
fn main() {
    println("Hello, Spid3r!");
    return 0;
}
```

```bash
$ spid3r run hello.matrix
Hello, Spid3r!
```

---

## ⚡ Highlights

- **Zero-Dependency Core**: Pure Rust. Builds in seconds without LLVM or external C libraries.
- **Dual Engine Parity**: Reference tree-walking interpreter and optimizing bytecode VM with identical output.
- **Rich Standard Library**: Built-in HTTP/TCP, Web framework, DataFrames, ML/Stats, CSV, JSON, Regex, and 20+ data structures.
- **Editor Support**: Language Server Protocol (`spid3r lsp`) with diagnostics, hover, go-to-definition, and formatting — reusing the exact same checker as `spid3r check`.
- **Multi-Target**: Run via CLI, compile to native machine code (LLVM), embed in Rust, bundle as Desktop GUI, or export as standalone offline WebAssembly.

---

## 🚀 Quick Start (Easy to Use)

### 1. Installation

**Linux / macOS (One-line installer):**
```bash
curl -fsSL https://raw.githubusercontent.com/needyamin/spid3r/main/install.sh | sh
```

**Windows:**
Download and run `Spid3r-Setup-<version>.exe` from the latest release, or build from source.

**From Source (Cargo):**
```bash
# Build & install core toolchain into ~/.cargo/bin
cargo install --path . --no-default-features
```

### 2. Common CLI Commands

| Task | Command |
|---|---|
| **Run a script** | `spid3r run app.matrix` |
| **Run with Bytecode VM** | `spid3r run --engine vm app.matrix` |
| **Check types & syntax** | `spid3r check app.matrix` |
| **Interactive REPL** | `spid3r repl` |
| **Format code** | `spid3r fmt app.matrix` (or `spid3r fmt --check`) |
| **Debug script** | `spid3r debug app.matrix --break 10` |
| **Language server (editor)** | `spid3r lsp` (speaks LSP over stdio) |
| **Compile to bytecode** | `spid3r compile app.matrix -o app.matrixc` |
| **Execute bytecode** | `spid3r exec app.matrixc` |
| **Bundle app** | `spid3r bundle app.matrix --target wasm --single-file` |

---

## 💡 Language at a Glance (Easy Understanding)

Spid3r combines readable C/Rust-like syntax with dynamic flexibility and optional gradual types.

```text
// Types are optional; functions are first-class
fn add(a: int, b: int) -> int {
    return a + b;
}

// Classes, inheritance, and interfaces
interface Greeter {
    fn greet(self) -> string;
}

class User implements Greeter {
    public let name;
    fn new(name) { self.name = name; }
    public fn greet(self) -> string {
        return "Hello, " + self.name;
    }
}

// Error handling: try/catch/finally & ok/err results
fn safe_divide(a, b) {
    if b == 0 { return err("division by zero"); }
    return ok(a / b);
}

fn main() {
    // Built-in collections & functional methods
    let numbers = list(1, 2, 3, 4, 5);
    let evens = numbers.filter(fn(x) { return x % 2 == 0; });
    println(evens); // [2, 4]

    // Associative maps & 1D/2D linear algebra
    let user_map = map("id", 101, "role", "admin");
    let v = vector([1.0, 2.0, 3.0]);
    let m = matrix(2, 2, 1.0); // 2x2 matrix

    // Structured errors
    try {
        let res = safe_divide(10, 2).unwrap_or(0);
        println("Result: {}".format(res));
    } catch (e) {
        println("Caught: " + e);
    } finally {
        println("Cleanup done.");
    }

    // Lightweight task spawning & channels
    let ch = channel();
    spawn fn() { ch.send("pong"); };
    println("Received: " + ch.recv());

    return 0;
}
```

### First-Party Standard Modules (`stdlib/`)
- **`stdlib/web.matrix`**: Full web framework with routing, cookies, templates, and JWT.
- **`stdlib/assert.matrix`**: Test assertions (`eq/ne/is_true/near/contains/throws/is_ok/is_err`,
  `done()` returns the failure count as the exit code) — a pytest/PHPUnit-shaped mini suite.
- **`stdlib/toml.matrix`**: TOML-subset parser (tables, arrays of tables, dotted keys, arrays,
  inline tables, literal strings) returning nested maps.
- **`stdlib/semver.matrix`**: Semantic-version parsing, spec-correct comparison (prerelease
  ordering included), and comparator-range `satisfies`.
- **`stdlib/uuid.matrix`**: RFC 9562 version-4 UUIDs from the runtime entropy source.
- **`stdlib/gen.matrix`**: Generators (`gen`/`next`/`collect`/`take`/`map`/`filter`/`range_gen`/
  `close`) — lazy, composable streams; infinite producers cost nothing until consumed.
- **`stdlib/dataframe.matrix`**: Pandas-style columnar table operations (filter, sort_by, group_by, inner_join).
- **`stdlib/stats.matrix`** & **`stdlib/ml.matrix`**: Regression, classification, clustering, PCA, and distributions.

---

## 🛠️ Development (Easy to Develop)

### Build from Source
```bash
# 1. Fast zero-dependency core (default for local development)
cargo build --no-default-features

# 2. Core + Desktop Webview GUI
cargo build --no-default-features --features gui

# 3. Full build with LLVM native compiler (requires LLVM 18.1+)
cargo build
```

### Testing
```bash
# Run all core unit and integration tests (696 total: 680 core + 10 GUI + 6 AOT).
# Canonical counts: bash tools/count_tests.sh
cargo test --no-default-features

# Differential engine verification (Tree vs VM vs Bytecode on all examples)
cargo run --no-default-features -- test

# Run GUI bridge tests
cargo test --no-default-features --features gui --test gui_tests
```

### Formatting & Linting
```bash
cargo run --no-default-features -- fmt --check
cargo clippy --no-default-features --all-targets -- -D warnings
```

---

## 🔍 Debugging (Easy to Debug)

Spid3r includes a dedicated, interactive VM line debugger.

### 1. Interactive Debugger
```bash
# Start debugging and break at line 15
spid3r debug app.matrix --break 15
```

At the `(spid3r)` prompt:
- `b <line>` / `d <line>`: Add / remove breakpoint
- `n` / `next`: Step to next line (step over)
- `s` / `step`: Step single instruction (step into)
- `c` / `continue`: Resume execution until next breakpoint
- `f` / `finish`: Run until current function returns
- `locals`: Inspect local variables in the current stack frame
- `bt` / `backtrace`: Print the call stack
- `l` / `list`: View surrounding source code
- `q` / `quit`: Exit debugger

### 2. Static Analysis & Diagnostics
```bash
# Check syntax, privacy, and types without executing
spid3r check app.matrix
```

### 3. Bytecode Disassembly
```bash
# Inspect compiled bytecode instructions and optimizer output
spid3r compile app.matrix --dump
```

### 4. Differential Verification
If an engine behavior seems unexpected, run both engines to compare:
```bash
spid3r run --engine tree app.matrix
spid3r run --engine vm app.matrix
```

---

## 📦 Packaging & Release (Easy to Release)

### 1. Release Binaries
```bash
# Build optimized release binary
cargo build --release --no-default-features

# Windows NSIS installer: Automatically outputs `Spid3r-Setup-<version>.exe`
cargo build --release
```

### 2. Application Bundler (`spid3r bundle`)
Package any Spid3r app into standalone target distributions:

```bash
# Single-file offline HTML app (embeds WASM runtime + bytecode, zero setup)
spid3r bundle app.matrix --target wasm --single-file -o dist/app.html

# Web deployment folder (index.html, spid3r.js, spid3r.wasm)
spid3r bundle app.matrix --target wasm -o dist/web/

# Desktop bundle with launch scripts
spid3r bundle app.matrix --target desktop -o dist/desktop/

# Android Studio project scaffold
spid3r bundle app.matrix --target android -o dist/android/
```

### 3. Precompiled Bytecode Distribution
Distribute apps without exposing raw source code:
```bash
spid3r compile src/main.matrix -o dist/app.matrixc
spid3r exec dist/app.matrixc
```

### 4. Projects & Package Management
```bash
# Initialize a new project
spid3r init my_project && cd my_project

# Install/vendor dependencies from spider.toml into vendor/ (writes spider.lock)
spid3r install

# Build release bytecode into target/<package>.matrixc
spid3r dist

# Pack into a POSIX ustar tarball
spid3r pack -o dist/my_project.tar
```

---

## 📚 Documentation & Resources

| Guide | Description |
|---|---|
| [Interactive Developer Guide](https://needyamin.github.io/spid3r/doc.html) | Single-page searchable guide with full API reference and cheatsheets |
| [WebAssembly Playground](docs/html/playground.html) | Run and test Spid3r directly in your web browser |
| [Language Grammar](docs/GRAMMAR.md) | Formal EBNF grammar and precedence rules |
| [Standard Library Reference](docs/STANDARD_LIBRARY.md) | Complete list of built-in functions and modules |
| [Data Structures](docs/DATA_STRUCTURES.md) | Methods and usage for all 20+ built-in collections |
| [Rust Embedding Guide](docs/EMBEDDING.md) | How to embed Spid3r as a scripting engine in Rust |
| [Native LLVM Backend](docs/NATIVE.md) | Compiling `.matrix` directly to native executables |
| [Webview GUI Guide](docs/GUI.md) | Building native desktop GUI apps |
| [Garbage Collection](docs/GC.md) | Memory management and cycle collector architecture |
| [Example Programs](examples/) | 24 runnable sample programs (+1 shared library module) |
| [Demos](demo_apps/) | Native Windows GUI Calculator & Web Calculator |

---

## 📄 License

Spid3r is distributed under the [MIT License](Cargo.toml).
