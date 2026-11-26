# Spid3r Integration Test Suite (`tests/`)

Integration test crates live under domain subdirectories (`Cargo.toml` sets `autotests = false` with explicit grouped `[[test]]` entries), so every existing `cargo test --test <name>` command stays unchanged while keeping related suites easy to browse.

## Directory Layout

| Folder | Suites | Focus |
| :--- | :---: | :--- |
| **`tests/language/`** | 25 | Lexer, parser, syntax, type checking, generics, OOP, and modules. |
| **`tests/engines/`** | 12 | Tree interpreter oracle, bytecode VM, optimizer, differential `engine_matrix`, WASM, and Rust embedding. |
| **`tests/native/`** | 8 | YamLLVM native AOT backend (`--features native`), FFI (`extern fn`), freestanding cross-targets, GC portability, and single-file apps. |
| **`tests/runtime/`** | 18 | YamGC cycle collection, concurrency (`spawn`/channels/mutex/atomics), scheduler limits, timers, event loop, and execution budgets. |
| **`tests/stdlib/`** | 17 | Data structures, collections, codecs, cryptography builtins, files/paths, regex, Unicode/tz, and SQLite/PostgreSQL/MySQL drivers. |
| **`tests/net/`** | 8 | TCP/UDP/HTTP, `native-tls` (`--features tls`), SMTP/FTP, and web framework stacks. |
| **`tests/tooling/`** | 15 | CLI dispatch & component aliases, formatter (`fmt`), `doc`, LSP, debugger, profiler, `control`, GUI (`--features gui`), `spkg`, and bundling. |
| **`tests/security/`** | 12 | `.matrixc`/staticlib/NSIS/MSIX hardening (`artifact_hardening`), security audits, path/permission filters, and fuzz smoke suites. |
| **`tests/regressions/`** | 14 | Targeted bug-fix and edge-case regression suites. |
| **`tests/common/`** | — | Shared Rust test helpers (`tls_fixture.rs`). |
| **`tests/data/`** | — | Fixture certificates and generated Unicode/TZ test vectors (`unicode_tz_cases.matrix`). |

## Running Tests

```bash
# Default core suite (zero external deps)
cargo test --no-default-features

# Native AOT + TLS + GUI suites
cargo test --features native,tls
cargo test --no-default-features --features gui --test gui_tests

# Target a single suite directly by name
cargo test --no-default-features --test engine_matrix
cargo test --no-default-features --test stage2_concurrency
cargo test --features native --test artifact_hardening
```
