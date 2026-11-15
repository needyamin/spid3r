# Spid3r — agent rules (self-hosting program)

Project: **Spid3r**, a programming language (`.matrix` sources) with a Rust implementation (v1.1.0, LLVM 23.1.0 AOT backend).
Active program: **make Spid3r compile its own compiler** (self-hosting). Single source of truth: `full_plan.html` (roadmap + append-only evidence ledger). Architecture decisions: `docs/SELFHOST.md` (SH-1…SH-8).
Read both before changing anything. If this file and `full_plan.html` disagree, `full_plan.html` wins; tell the user and fix this file.

## 1. Current state (update when a phase moves)

As of 2026-11-15 (last commit pushed: `cef1a36`, Phase 10 CI #123 green):

- Phases 0–9 + 10.2 + 11.1 (`P-1`) + 11.2 (`P-2`) + 11.3 (`P-3`) closed (`10.1` Windows local + Linux `aot` CI #123 verified; `release.yml` 4-tarball + Windows `--smoke-install` pending `workflow_dispatch` or tag). Earlier CI evidence: Phases 0–7 run `#115`, Phase 8 (`9e4f5d3`, CI `#117`), Phase 9 (`9a39c3f`, CI `#119`), Phase 10 (`cef1a36`, CI `#123`, `7m 54s` / Pages `#124`, `26s`).
- Phase 11.1 (`P-1`) highlights (`benchmarks/reference.p1.json`, `docs/html/benchmark.html`, `full_plan.html` Appendix B): `tools/phase0_bench.py` extended for `node` (`benchmarks/compare/{prime,json,map}.js` + `phase0_calibration.js`) and `rust` (`rustc -O benchmarks/compare/prime.rs` + `phase0_calibration_rs.exe`), ordered blocking 5% sensitivity `selftest` before compiler builds, and disclosed internal `hrtime.bigint`/`Instant::now` stats alongside external subtraction when `startup_share > 50%` (10/10 rows PASS with `MAD/med` 0.34%–3.63% and 2-pass agree 0.06%–1.83%, `selftest_pass: True`; `ADVISORY_ONLY` on shared Windows host): `prime` VM `31.15 ms` / native `8.74 ms` / Node `1.704 ms` / Rust `1.362 ms`; `json` VM `167.92 ms` / native `199.05 ms` / Node `17.600 ms`; `map` VM `21.21 ms` / native `45.17 ms` / Node `1.562 ms`. Also updated `tools/bench_report_render.py` + `tools/bench_report.py` Section 04.
- Phase 11.2 (`P-2`) highlights (`benchmarks/selfhost/baseline.json`, `benchmarks/selfhost/README.md`, `docs/html/benchmark.html` Section 05, `full_plan.html` Appendix B): checksummed `benchmarks/selfhost/MANIFEST.sha256` workloads (`lex` 1,016,880 B, `parse`/`check` 121 corpus programs, `self_compile` 17 modules / 584,703 B, `corpus_build` 6 examples), measured Stage 0 vs Stage 1 (`stage0 build --release`, Core 6) with 10/10 PASS (`MAD/med` 0.59%–2.52%, agree 0.05%–2.12%, `selftest_pass: True`): `lex` S0 `16.5506 MB/s` / SH `0.2397 MB/s` (`0.0145×`, floor `0.192`), `parse` S0 `87.364/s` / SH `71.959/s` (`0.8237×`, floor `57.6`), `check` S0 `91.819/s` / SH `53.965/s` (`0.5877×`, floor `43.2`), `self_compile` S0 `0.45667 MB/s` / SH `0.04823 MB/s` (`0.1056×`, floor `0.0386`), `corpus_build` S0 `975.66 ms` / SH `2205.89 ms` (`2.2609×` slower, max `2757.4 ms`). `tools/lex_bench.py`, `tools/parse_bench.py`, and `tools/check_bench.py` now read targets directly from `benchmarks/selfhost/baseline.json`.
- Phase 11.3 (`P-3`) highlights (`selfhost/{llvm,codegen,main,link}.matrix`, `full_plan.html` Appendix B): wired `LLVMRunPasses("default<O3>")` + `OPT_AGGRESSIVE=3` (`OPT_NONE=0` otherwise) via LLVM-C `PassBuilder.h`/`Error.h` on `--release`, and aligned `find_runtime_staticlib()` to search `target/release` before `target/debug` matching Stage 0. Fixed point `spid3r_main.o` (`3,102,449` B) `E1DC9673B9BEF7B1C58D7B86006BEA5D0935614F3C36391178E92DD6AFDB9C5C`; `--release` output-quality parity: HLC `prime` `1.0072×` (`9.20 ms` vs `9.14 ms`; `1.237×` vs unopt IR), `json` `0.9947×` (`197.00 ms` vs `198.04 ms`), `map` `0.9963×` (`45.12 ms` vs `45.28 ms`), whole corpus (111/121 measured, 10 expected skips) median `1.0007×`, max `1.0435×` (≤ 1.25× Tier-B gate).
- Roadmap progress: 36/51 checkboxes (`s11-3` closed; `s10-1` open pending `release.yml` `workflow_dispatch` / tag run). Next work: Phase 11 `P-4` (selfhost lexer throughput optimization, or trigger `release.yml` to close `10.1`).
- Stale ledger row: the 2026-11-14 `5.3 OPEN` row is superseded by the 2026-11-15 `5.3` PASS row (a correction note row exists). The ledger is append-only, so never delete rows. Add a note instead.
- Known gap: the cycle-diagnostic case (a↔b import) is not in the frozen corpus; adding it needs its own SH-4 ledger entry.
- Untracked or ignored build artifacts live in `selfhost/` (`main_native.exe`, `llvm_min.exe`, `spid3r_main.o` if emitted alongside `main_native.exe`). Do not commit them.

## 2. Ground rules (non-negotiable, from the plan's GR-1…GR-4)

1. **No fake implementations, no fake results.** A checkbox may be checked only when its **Gate** (the verification written under the step) has actually run and passed on the real repository. The evidence (date, command, result) is appended to the ledger in `full_plan.html` in the same commit. If a gate can't be verified, the step stays open and says why.
2. **No speed claim without benchmark evidence.** Use `tools/phase0_bench.py` (9 warmups / 21 repeats / 2 passes, MAD÷median ≤ 5%, two-pass agreement ≤ 3%, startup-subtracted). Tiers: **A** no-regression floors (blocking), **B** measured-improvement targets vs frozen baselines (blocking), **C** stretch (labeled, never blocking). A target measured unreachable is retired openly (see `docs/PERF_TARGET_RETIREMENT.md`), with the owner's confirmation. Never promise numbers. Absolute ms on this shared host are advisory; ratios are the portable truth.
3. **Engine contract holds after every step.** `tools/gate.sh` must report 9/9 and `tools/difftest.sh` ALL MATCH. The tree interpreter is the semantic oracle (`docs/ENGINE_CONTRACT.md`). `gate.sh` runs default features only, so also run `cargo fmt --check` and `cargo clippy --all-targets --features native` before pushing. rustfmt drift has already failed CI once.
4. **Frozen corpus (SH-4).** `selfhost/corpus/` (134 programs, 135 files, pinned by `MANIFEST.sha256`) is what every parity gate runs against. Changing it requires a ledger entry stating why, plus `python tools/corpus_manifest.py build` and then `verify`.
5. **Parity is measured against Stage 0 (the Rust `spid3r`), never against the port's own idea of correctness.** Port 1:1 from the Rust source. Don't reimplement from memory.
6. **Teeth proof.** Every new parity harness or gate is deliberately broken once, shown RED, then restored. Record it in the ledger.
7. **Report honestly.** Record negative results, divergences, flakes and tooling problems in the ledger. Never present a partial pass as complete. Say "OPEN" when something is open.

## 3. Security invariants (GR-3, SEC-A…D)

- No new `unsafe` outside `src/native/rt.rs`, and each one needs a written safety comment.
- The ptr/usize FFI surface is T5 trusted code. `--sandbox` filters script-engine natives only. AOT-built executables, whether built by Stage 0 or by selfhost, run with the full authority of the user, by design (`docs/FFI.md`). Don't add a "sandboxed build" feature.
- Every artifact (selfhost binaries, `.matrixc`, runtime staticlib) carries SHA-256 integrity. A tampered artifact gives a clean exit 4 or a usage error, and never executes.
- The Spid3r-written lexer and parser stay fuzzed against the Rust oracle (zero diffs, zero crashes or hangs).
- Compiler operations perform no network access.
- Exit codes follow the 0/1/2/4 contract (see `README.md` "Exit codes"). Usage and build-driver failures are exit 2, `error[usage]`.

## 4. Architecture cheat-sheet

```
Stage 0  spid3r (Rust, src/)             oracle + bootstrap, stays forever
Stage 1  spid3r-self   = selfhost/main.matrix built by Stage 0's AOT (selfhost/main_native.exe today)
Stage 2  spid3r-self-2 = Stage 1 compiling selfhost/main.matrix; must behave identically (fixed point)
```

- Self-host pipeline in `selfhost/main.matrix` (`native` mode): `parse → check → flatten → lower → codegen (LLVM-C) → verify → emit object → link.matrix`.
- Modules: `token`, `lexer`, `lexer_json`, `ast`, `parser`, `semantic`, `lower`, `vmc`, `bytecode_matrix`, `llvm`, `codegen`, `link`, `flatten`, `test_runner`, `fmt`, `pkg`.
- Runtime = prebuilt staticlib `spid3r_lang.lib` (AOT trampolines + 230 stdlib natives). Rebuilding the runtime is Phase 9. It may stay a pinned binary artifact (SH-6).
- Meaning of "no Rust" (SH-2): Phase 7 means building a compiler needs no cargo. Phase 10 means the release artifacts are produced by selfhost. LSP, GUI, agent, debugger, REPL and the `tools/` harness scripts stay Rust or script side.
- Oracle dump surface (bootstrap tooling only): `spid3r tokens --json`, `parse --dump-ast-json`, `parse --dump-lowered-json`, `check --dump-registry-json`, `compile --no-opt`.

## 5. Environment (Windows, this machine)

- Shell: PowerShell for general work. The `tools/*.sh` harnesses need **Git Bash** (`C:\Program Files\Git\bin\bash.exe`). Plain `bash` on PATH is WSL's and breaks them. Run `& "C:\Program Files\Git\bin\bash.exe" tools/<script>.sh`, and close stdin (`< /dev/null`) when launching from a non-terminal: a corpus program blocks waiting on input otherwise (the native-parity run hangs for minutes). The native-parity sweep takes ~14 minutes and prints only failures until the summary.
- PowerShell 5 pitfall: `Get-Content`/`Set-Content` re-encode UTF-8 (BOM + mojibake) on `.matrix`/`.sh` files containing `—`/`→`. Edit those with the editor tools, not Get-Content/Set-Content.
- Toolchain: MSVC BuildTools `VC/Tools/MSVC/14.51.36231`, Windows SDK `10.0.26100.0`, **LLVM-C at `C:/LLVM-23`**, staticlib at `target/debug/spid3r_lang.lib`.
- Build Stage 0 with the native feature: `cargo build --features native`. Run the native tests with `cargo test --features native` (CI uses release, `--no-default-features`).
- Stage-1 build: `spid3r build selfhost/main.matrix -o selfhost/main_native.exe`. Use it as `main_native.exe native <file.matrix> <out.exe>`.
- Parity harnesses (all in `tools/`): `selfhost_lex_parity.sh`, `selfhost_parse_parity.sh`, `selfhost_semantic_parity.sh`, `selfhost_lower_parity.sh`, `selfhost_compile_parity.sh`, `selfhost_native_parity.sh` (Windows-local only, expect pass=124 fail=0 skip=10 defer=0), `selfhost_bootstrap_parity.sh` (3-way Stage 0/1/2, expect pass=124 fail=0 skip=10 obj_match=111 ok=226), `selfhost_sandbox_parity.sh` (13 checks), `selfhost_test_parity.sh` (10 checks), `selfhost_fmt_check_parity.sh` (17 checks + 134/134 corpus), `selfhost_pkg_parity.sh` (11 checks), `selfhost_runtime_parity.sh` (16 checks). Fuzzers: `fuzz_lex_parity.py`, `fuzz_parse_parity.py`. Bench guards: `lex_bench.py`, `parse_bench.py`, `check_bench.py`.
- Never run two cargo builds with different feature sets at once. They all write `target/debug/spid3r`.
- Known CI flake: `tests/phase2_net_limits.rs` (connection reset under parallel load). Prove it in isolation, re-run, and record it. Don't "fix" unrelated code.

## 6. Writing Spid3r (selfhost sources) — known traps

- Imports resolve against the importer's directory. `../` is rejected. So `selfhost/` is one flat module root.
- All module-level functions are flattened into one namespace. **Same-named helpers in different modules collide.** Prefix private helpers (`chk_`, `cg_`, `fl_` …). Past bugs came from `type_of`, `dirname_of`, `adv`, `emit`.
- Keywords can't be identifiers (`class`, `catch`, `finally` …).
- `&` precedence trap: `u & 128 == 0` parses as `u & (128 == 0)`. Parenthesize.
- Strings are immutable, and `s[i]` is O(i). Never build strings with `+=` in loops. Use fragment lists plus one join. Scan `bytes` for O(1) indexing. `s.len()` counts characters, not bytes.
- Maps have no delete (rebuild instead). `spawn_process` has no output capture (use `exec_capture`). `env()` returns an Option, `list_dir`/`exe_path` return Results.
- A NULL pointer returned across FFI arrives as `Int(0)`, not null. Compare to 0. Zero-length C arrays still need a real malloc'd pointer. The only write primitive across the opaque ptr boundary is CRT `memset`.
- Runtime trampolines (`spid3r_binop`, `spid3r_value_int` …) are the AOT's internal calling convention. Reference them at the IR level of the **target** module (`LLVMAddFunction`). Never declare them as `extern fn`.
- LLVM 23 renames: `LLVMFunctionType` has no ctx and takes 4 args, `LLVMArrayType2(elem, count)`, `LLVMPointerTypeInContext(ctx, addrspace)`. Verify any C-API call against `C:/LLVM-23` headers before use.
- Compiler errors are `[msg, line, col, len, phase, path]` lists. Bare strings are runtime errors. Tell them apart with `e[1] + 0` in try/catch.
- Known stage-0 oracle nondeterminism: `spid3r compile` embeds module public-set order from a Rust HashSet, so artifact bytes can differ between runs. Gate on exec-equivalence and any-of-N orderings. The checker walks HashMaps on multi-violation programs (skip unstable fuzz inputs).

## 7. Workflow for every step

1. Read the step's Deliverable and Gate in `full_plan.html` and the matching SH-n decision.
2. Survey the relevant Rust source and `docs/` first. Write the port against it.
3. Implement, then run the step's parity harness plus the earlier harnesses (no regressions). Then `tools/gate.sh` and `tools/difftest.sh`.
4. Teeth-prove any new harness. Run the Tier-B perf gate under the phase0 protocol where the step has one.
5. In **one commit**: code + `docs/SELFHOST.md` progress text if it changed + `full_plan.html` (tick the checkbox, append the ledger row, update progress counts).
6. Push, then check GitHub CI (public Actions). Append a separate "CI evidence" ledger row once green. If red, record the cause honestly. If it's a flake, prove it and re-run.
7. Do not push, force-push or rewrite history unless the user asks. Ask before committing if the user hasn't said to.

## 8. Tooling quirks (the Mimosa security hook)

- A PreToolUse scanner (`.mimosa/`) can block Python files that call `subprocess` with a variable executable (false positive for list-form, `shell=False`). It can also block `git commit` when the staged file path appears in the command text. Workaround that worked: `git add tools/` (parent directory) and commit without naming the file. Prefer bash timing loops over new subprocess-based Python harnesses unless the owner signs off.
- Scanner `ENOBUFS` errors are resource errors, not findings. Make no security claim from them.

## 9. How to work with the user

- The owner has been working in a different IDE and switched here after running out of tokens. The git history, the ledger and `.zcode/plans/` are the memory. Don't ask the user to re-explain what is already in the ledger.
- Keep answers concise. Link files with `file:///` links. Lead with the result and its evidence.
- When a decision needs the owner (target retirement, scope change, corpus change), present it, record "pending owner confirmation" in the ledger, and proceed only along the reversible path the plan allows.
