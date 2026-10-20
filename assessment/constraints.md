# Assessment constraints — Spid3r (GaliLang workspace), 2026-10-20

User requirements, quoted verbatim:

1. "Inspect the actual Git state, project structure, Rust toolchain, workspace configuration, dependencies, and existing project instructions."
2. "Identify the real lexer, parser, interpreter, VM, compiler, garbage collector, scheduler, package manager, standard library, tests, and fuzzing targets."
3. "Establish a baseline using available, appropriate formatting, compilation, lint, and test commands. Do not modify source files during this initial assessment."
4. "Identify existing failures separately from confirmed defects."
5. "Do not assume that documented commands, modules, execution engines, or test counts exist. Verify them."
6. "Rank the ten most important verified engineering concerns by severity, confidence, and user impact. If fewer than ten can be verified, report fewer."
7. "Recommend a staged remediation plan, beginning with the highest-risk confirmed issue."
8. "Return exact evidence, affected files, actual command results, limitations, and the next recommended task."
9. "Do not claim to have completed an audit of files you have not inspected. Do not invent findings or test results."
10. "Wait for the assessment to finish before performing source-code modifications."

Inferred items:

- [inferred] Output language: English (user's message language).
- [inferred] Final deliverable: a written audit report (workspace file) plus chat summary with exact evidence (commands + results), file paths, limitations, next task.
- [inferred] No source modification anywhere in the repo during this run (user gate: wait for assessment to finish first).
- [inferred] Child agents are read-only inspectors: no cargo/rustc/toolchain invocation, no file edits outside `assessment/findings/`.
- [inferred] Scratch/notes area: `assessment/` (untracked; never committed; not part of the audited deliverable).
- [inferred] Dynamic baseline (build/clippy/test/differential/fmt/wasm/GUI) is run by the coordinator only, to avoid target-dir lock contention.

Established facts (verified by coordinator, 2026-10-20):

- Project is `spid3r` v1.0.0, edition 2021, a programming language in Rust (core zero-dependency; optional `gui` [wry+tao] and `native` [inkwell/LLVM 18.1+] features, both default-on but excluded from the core baseline).
- rustc/cargo 1.98.1 on Windows; `profile.dev incremental = false` (OneDrive corruption workaround documented in Cargo.toml).
- Git: branch `main` tracks `origin/main`; uncommitted modifications: `.gitignore`, `benchmarks/results.json`, `docs/html/benchmarks.html`, `src/interpreter/mod.rs`, `src/parser/mod.rs`, `src/sched/mod.rs`, `tools/bench_compare.py`, deletion of `benchmarks.html`. Source diffs are `#[cfg(not(target_arch = "wasm32"))]` attribute changes (wasm-target fix) — must be validated by the wasm build.
- src layout (45 .rs files, 1,270,814 bytes): lexer, parser, ast, diagnostics, fmt.rs, semantic, interpreter, environment, value (+collections, gc), vm (bytecode/compiler/opt/debug), sched, modules, pkg, stdlib (mod/net/regex/csv/datetime), cli.rs, repl, embed.rs, bundle.rs, wasm.rs, gui, native.
- tests/: 32 integration test files; fuzz/: 3 targets (fuzz_lexer, fuzz_parser, fuzz_bytecode).
- README claims to verify: 447 Rust tests total (418 core + 29 GUI/AOT-headless), 25/25 differential example programs, 23 examples, 8 benchmarks, "engines produce identical output", zero third-party deps in core, package manager with lockfiles+SHA-256, path-traversal-protected ustar archiver.
- CI (.github/workflows/ci.yml): clippy `--no-default-features --all-targets -- -D warnings`, `cargo test --no-default-features`, `cargo run --no-default-features -- test`, `cargo run --no-default-features -- fmt --check`, plus gui/wasm jobs.
- Prior audit artifacts exist (`audit_notes/*.md`, `docs/AUDIT_REPORT.md`) — prior claims, to be re-verified, not trusted.
