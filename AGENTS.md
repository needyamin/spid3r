# Spid3r — Strict Agent Rules & Language Scope Constitution

Project: **Spid3r**, a general-purpose programming language (`.matrix` sources) with a Rust Stage-0 implementation (v1.5.4, LLVM 23.1.0 AOT backend) and a self-hosted compiler (`selfhost/`, `spid3r-self`).
Architecture decisions: `docs/selfhost/SELFHOST.md` (SH-1…SH-8). Removed non-language subsystems: `removed.txt`.

---

## 0. STRICT LANGUAGE SCOPE & ANTI-HALLUCINATION RULES (NON-NEGOTIABLE)

Spid3r follows the **core programming language model of PHP, Python, and Java**: a programming language provides a compiler/runtime, memory & type system, standard library, and developer tooling — **NOT** operating systems, container engines, remote admin servers, custom crypto stacks, or third-party-ecosystem frameworks.

### 0.1 What BELONGS in Spid3r Core (Allowed)

| Category | Allowed in Core (PHP / Python / Java standard) |
| :--- | :--- |
| **Language & Compilers** | Lexer, parser, AST, type checker/semantic pass, generics & monomorphization, lowering, bytecode compiler, VM, tree-walk oracle, LLVM AOT backend, WebAssembly (`wasm32`) target, self-hosted compiler (`selfhost/`). |
| **Runtime & Memory** | Deterministic RC + cycle GC, concurrency (`spawn`, channels, mutex), C FFI (`extern fn`), structured errors (`try`/`catch`, `Result`, `Option`), local `spid3r.ini` configuration & capability filters (`--sandbox`, `Engine::deny`). |
| **Standard Library (`stdlib/`, `src/stdlib/`)** | Core data structures, math, strings, Unicode, datetime/IANA tz, regex, filesystem/paths, OS/process/env, TCP/UDP/HTTP/WebSocket/TLS networking, JSON/CSV/TOML/YAML/XML/MsgPack/CBOR/Protobuf/compression, SQLite/PostgreSQL/MySQL drivers, SMTP/FTP, testing (`assert`, `mock`, `testkit`), and standard hashing (`sha256`, `sha512`, `hmac`, `pbkdf2`, `argon2`, `bcrypt`, `ed25519`). |
| **Developer Tooling** | CLI (`run`, `check`, `compile`, `build`, `exec`, `disasm`, `test`, `bench`, `fmt`, `doc`, `lsp`, `repl`, `debug`, `bundle`, `pack`, `unpack`), package manager (`init`, `install`, `add`, `remove`, `tree`, `verify`, `search`, `publish`), `spid3rinfo()`, local `spid3r control` / `spid3r-control` panel, and optional desktop `gui` (`wry`/`tao`). |
| **Kept by Owner Decision** | `spid3r_web/` (separate web framework directory — never couple compiler/VM/selfhost parity to it). |

### 0.2 What is STRICTLY FORBIDDEN (NOT Part of a Programming Language)

**NEVER add, re-introduce, or propose adding any of the following into the Spid3r language repo** (neither PHP, Python, Java, nor Swift bundle these in their core language):

1. **NO OS Kernels or Bootloaders** (`kernel/`, `Spid3rOS`, bare-metal drivers/schedulers). *The compiler may emit freestanding objects (`--freestanding`), but an OS belongs in a separate repository.*
2. **NO OCI Container Engines or Orchestrators** (`src/oci/`, `src/container/`, Docker clones, Linux namespace/cgroup/overlayfs syscall shims).
3. **NO Remote Management Agents or Admin Daemons** (`src/agent/`, `spid3r agent`, `spid3r admin`, paired-device TCP servers, remote config-push protocols).
4. **NO Heavy Domain Libraries in Core `stdlib`** — NO Machine Learning (`ml`), NO DataFrames (`dataframe` / pandas clones), NO Redis/NoSQL drivers (`redis`), NO SSH/SFTP wrappers (`ssh`), NO ORMs, NO game engines, NO AI/blockchain SDKs. *Like `scikit-learn`, `pandas`, `redis-py`, `paramiko` (Python), `predis`/`Laravel` (PHP), or Maven libs (Java), these belong in **external packages** installed via `spid3r add` / `spider.toml`.*
5. **NO Unvetted Hand-Rolled Cryptography** (`src/crypto/`, custom ChaCha20, Poly1305, AES, TLS, PAKE, or key-exchange protocols). Use OS/vetted libraries (`native-tls`) for encrypted transport.
6. **NO Embedded WASM or Native Plugin Hosts** (`src/plugin/`, `wasmi` inside the CLI). External CLI tools use `spidr-<name>` on `PATH`; Rust hosts use `spid3r_lang::embed::Engine`; native libraries use `extern fn` (FFI).
7. **NO Unrequested Scope Creep** — Do not invent new CLI subcommands, Cargo features, `spid3r.ini` keys, or stdlib modules unless the owner explicitly asks for them **and** they pass the test: *"Does PHP, Python, or Java put this in the core language?"*

### 0.3 Strict Anti-Hallucination Rules (Mandatory for Every AI Agent)

1. **Verify before claiming:** Never invent or assume file paths, function names, CLI flags, stdlib modules, config keys, or counts. Read the actual file or run the command first.
2. **Respect `removed.txt`:** Never cite, import, or recommend any removed subsystem (`kernel`, `oci`, `container`, `agent`, `admin`, `crypto`, `plugin`, `plugins-wasm`, `wasmi`, `redis`, `ml`, `dataframe`, `ssh`) as if it exists.
3. **Use real, current numbers (verify if changed):**
   - `26` embedded `.matrix` stdlib modules (`stdlib/`)
   - `254` built-in native stdlib functions (`src/stdlib/`)
   - `45` `spid3r.ini` schema keys across `12` sections (`src/config/schema.rs`)
   - `3` optional Cargo features: `gui`, `native`, `tls` (zero external deps with `--no-default-features`)
   - `136` frozen selfhost corpus programs (`137` files in `selfhost/corpus/MANIFEST.sha256`)
4. **No fake passes, no silent stubs:** Never mark a task/test as passing without running it. Never write fake/stub implementations to satisfy a test. If something fails or is blocked, report it honestly (`FAIL` / `OPEN`) and stop.
5. **No unrequested architectural changes:** If a requested fix seems to require adding a forbidden component from §0.2, adding a new Cargo dependency, or changing the language grammar/ABI, **STOP and ask the owner first**.

---

## 1. Current State

- Core toolchain: Stage 0 (`src/`, Rust v1.5.4) + Stage 1/2/3 self-hosted compiler (`selfhost/`, `spid3r-self`, LLVM 23.1.0 AOT).
- Phases 0–9 + 10.2 + 11.1–11.9 closed; `10.1` release workflow awaits owner `workflow_dispatch`/tag.
- Post-Phase 11 work completed: generics (`DP-1`), WS/HTTP/tar fuzzing (`R12.2`), Tracks 1–6 speed parity, local `spid3r.ini` (`45/45` keys) + `spid3r-control.exe`, and removal of the 6 non-language subsystems listed in `removed.txt`.
- Stale ledger rows in `docs/selfhost/SELFHOST.md` are never deleted (append-only ledger; add a correction note instead).
- Never commit untracked build artifacts in `selfhost/` (`main_native.exe`, `llvm_min.exe`, `spid3r_main.o`).

---

## 2. Ground Rules (Non-Negotiable, GR-1…GR-4)

1. **No fake implementations, no fake results.** Only claim a step is done when its real verification gate has run and passed on this repository.
2. **No speed claim without benchmark evidence.** Use `tools/bench/phase0_bench.py` (9 warmups / 21 repeats / 2 passes, MAD/median ≤ 5%, two-pass agreement ≤ 3%, startup-subtracted). Tiers: **A** no-regression floors (blocking), **B** measured targets (blocking), **C** stretch (advisory). Never promise numbers.
3. **Engine contract holds after every step.** `tools/ci/gate.sh` 9/9, `tools/ci/difftest.sh` ALL MATCH, `cargo fmt --check`, and `cargo clippy --all-targets --features native -- -D warnings`. The tree interpreter is the semantic oracle (`docs/ENGINE_CONTRACT.md`).
4. **Frozen corpus (SH-4).** `selfhost/corpus/` (`136` programs, `137` files, pinned by `MANIFEST.sha256`) is what parity gates run against. Any change requires a ledger entry explaining why + `python tools/selfhost/corpus_manifest.py build && python tools/selfhost/corpus_manifest.py verify`.
5. **Parity is measured against Stage 0 (Rust `spid3r`)**, never from memory. Any lexer/parser/semantic/lower/vm-emission change must be mirrored in `selfhost/*.matrix` (SH-3).
6. **Teeth proof.** Every new parity harness or gate must be broken once, shown RED, restored, and recorded.
7. **Report honestly.** Record negative results, divergences, flakes, and open items plainly.

---

## 3. Security Invariants (GR-3, SEC-A…D)

- **Unsafe boundary:** Do not add new `unsafe` outside the 14 audited FFI/host-shim modules (`src/native/{ir,link,llvm,rt}.rs`, `src/repl/mod.rs`, `src/runtime_info/mod.rs`, `src/stdlib/{db,entropy,eventloop,file,net,shutdown}.rs`, `src/value/mod.rs`, `src/value/collections/seq.rs`), enforced by `tools/ci/check_unsafe_safety.py` in `tools/ci/gate.sh`. Every `unsafe` block requires a written `// SAFETY:` comment.
- **Trust model:** `ptr`/`usize` FFI (`extern fn`) is T5 trusted code. `--sandbox` and `spid3r.ini` `[security]` switches are application-level policy filters for script engines (`tree`/`vm`) only — AOT executables run with full user authority by design (`docs/FFI.md`). Never claim `--sandbox` is an OS kernel sandbox.
- **Integrity:** `.matrixc` bytecode and `spid3r_lang` staticlibs verify SHA-256 digests (`exit 4` on tamper).
- **No network in compiler:** Compiler operations (`check`, `compile`, `build`, `fmt`, `doc`) perform zero network access.
- **Exit codes:** `0` ok · `1` compile/runtime/config-pin error · `2` usage/driver error (`error[usage]`: unknown command, invalid flags, or missing input file) · `4` corrupt/tampered `.matrixc` or runtime staticlib.

---

## 4. Architecture Cheat-Sheet

```text
Stage 0  spid3r (Rust, src/)             oracle + bootstrap + VM/tree/REPL/LSP/debug/GUI
Stage 1  spid3r-self   = selfhost/main.matrix built by Stage 0 AOT
Stage 2  spid3r-self-2 = Stage 1 compiling selfhost/main.matrix (bit-identical fixed point)
```

- Selfhost pipeline (`selfhost/main.matrix`): `parse → check → flatten → lower → codegen (LLVM-C) → verify → emit object → link.matrix`.
- Runtime staticlib: `spid3r_lang.lib` (`117` AOT trampolines + `254` stdlib natives; see `docs/selfhost/RUNTIME_ABI.md`).
- Retained in Rust (SH-2): Stage 0 oracle, runtime staticlib, VM, tree interpreter, LSP, GUI, `spid3r-control`, debugger, REPL, WASM target.

---

## 5. Environment (Windows, This Machine)

- **Shell:** PowerShell for general commands; `tools/*.sh` harnesses MUST run via **Git Bash** (`& "C:\Program Files\Git\bin\bash.exe" tools/<script>.sh < /dev/null`). Do not use WSL `bash`.
- **PowerShell 5 UTF-8 trap:** Never use `Get-Content`/`Set-Content` on `.matrix` or `.sh` files (adds BOM / corrupts UTF-8). Use file edit tools.
- **Toolchain:** MSVC BuildTools + Windows SDK + **LLVM-C at `C:/LLVM-23`**.
- **Cargo commands:**
  - `cargo build --features native`
  - `cargo test --features native`
  - `cargo check --no-default-features --all-targets`
  - `cargo fmt --check`
  - `cargo clippy --all-targets --features native -- -D warnings`
  - `node tools/docs/verify_doc_claims.js` and `node tools/docs/verify_dev_snippets.js`
- Never run two `cargo` builds with different feature sets concurrently.
- Known CI flake: `tests/net/phase2_net_limits.rs` (connection reset under parallel load) — isolate, re-run, record; do not "fix" unrelated code.

---

## 6. Writing Selfhost Spid3r (`selfhost/*.matrix`) — Known Traps

- `selfhost/` is one flat module root (`../` imports are rejected).
- All module-level functions flatten into one namespace — prefix private helpers (`chk_`, `cg_`, `fl_`, …) to prevent collisions.
- Keywords (`class`, `catch`, `finally`, …) cannot be identifiers.
- `&` precedence trap: parenthesize `(u & 128) == 0`.
- Strings are immutable and `s[i]` is `O(i)` — build strings via fragment lists + `.join("")`; scan `bytes` for `O(1)` indexing.
- Maps have no delete (rebuild instead). `spawn_process` has no capture (use `exec_capture`). `env()` returns `Option`; `list_dir`/`exe_path` return `Result`.
- NULL C pointers across FFI arrive as `Int(0)`, not `null`.
- Runtime trampolines (`spid3r_*`) are referenced via `LLVMAddFunction`, never declared as `extern fn`.
- LLVM 23 C-API: `LLVMFunctionType` takes 4 args (no ctx), `LLVMArrayType2(elem, count)`, `LLVMPointerTypeInContext(ctx, addrspace)`.

---

## 7. Workflow & Git Rules

1. Read relevant source and docs before editing.
2. Mirror compiler/semantic changes in `selfhost/` (SH-3) and run parity harnesses (`tools/selfhost_*_parity.sh`).
3. Run standing gates (`fmt`, `clippy`, `cargo test`, `tools/ci/gate.sh`, `tools/ci/difftest.sh`, doc claim checks) before claiming done.
4. **Never commit, push, force-push, or rewrite history without the owner explicitly asking.** Always ask before committing.
5. **Mimosa hook quirk (`.mimosa/`):** If `git commit` is blocked because a staged filename appears in the command text, stage by parent directory (`git add <dir>/`) and commit without naming the file.

---

## 8. Communication

- Keep responses concise and direct. Lead with results and verification evidence.
- Link referenced files with `file:///` Markdown links.
- When a scope, license, corpus, or target decision needs owner confirmation, stop and ask instead of guessing.
