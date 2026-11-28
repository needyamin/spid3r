# Security Policy

**A Spid3r script is code, not data.** By default, a `.matrix` / `.mt` program runs with the full authority of the user process that starts it — it can read files, open sockets, spawn child processes, and read environment variables, just like `sh` or `python`.

![Trust boundary: no gate by default, opt-in capability gate via --sandbox, spid3r.ini, or Engine::deny](docs/diagrams/security.svg)

The primary rule: **if you did not write or audit a script, treat it like a native executable from the same source.**

## Supported Versions

| Version | Supported |
|---|---|
| `1.5.x` (`v1.5.4`, current) | Yes |
| `1.1.x` | No |
| `1.0.x` | No |
| `< 1.0` | No |

## Reporting a Vulnerability

**Do not open a public GitHub issue.** Report security vulnerabilities privately via GitHub Security Advisories:

→ **https://github.com/needyamin/spid3r/security/advisories/new**

Alternatively, email security reports directly to **needyamin@gmail.com** with subject `[SECURITY] Spid3r vulnerability report`.

Please include:
- `spid3r version` (or `spid3r --version`) output and your OS/architecture
- The smallest `.matrix` / `.mt` program, `.matrixc` file, `spid3r.ini`, or protocol frame that reproduces the issue
- Expected vs. actual behavior

Response targets: acknowledge within **7 days**; ship a fix or mitigation for critical issues within **30 days**.

## In Scope

| Class | Examples |
|---|---|
| Memory safety | Panic or UB in the Rust Stage 0 host, `spvm` VM, `tree` interpreter, **YamGC** cycle collector, or `spid3r_lang` runtime reachable from a `.matrix` / `.mt` program |
| Capability escape | A native denied via `--sandbox`, `spid3r.ini` (`security.deny_*`, `security.disable_natives`, `modules.disable`), or `Engine::deny` / `deny_all()` (including `spid3rinfo`, `config_set`, `chdir`, and DB natives) still reachable from a script |
| Configuration pin bypass | Execution proceeding when an explicit `SPID3R_INI` pin is missing, unreadable, or malformed, or script `config_set` mutating frozen/resource-limit keys |
| Hostile input | Malformed v3 `.matrixc`, corrupt `--embed` payload trailer, hostile JSON/TOML/YAML/XML/MsgPack/CBOR/Protobuf/regex/HTTP/WebSocket/tar frames |
| Path traversal | `bundle`, `pack`, `unpack`, `install`, `tool install`, or `security.basedir` escaping its target directory |
| Crypto & auth misuse | Ed25519 signature verification, bcrypt, PBKDF2, Argon2, session signing, PostgreSQL SCRAM-SHA-256 or MySQL 8 authentication bugs, or `SPID3R_REGISTRY_TOKEN` leaking across redirects / non-HTTPS |
| Supply chain & runtime ABI | Bypassing `spider.lock` source/native SHA-256 verification, `spkg` signed metadata (`sha256*.txt.sig`, `manifest*.sha256.sig`, `registry.toml.sig`, `index.txt.sig`, `yanked.txt.sig`, `advisories.txt.sig`), pre-link FFI lock re-verification, or `spid3r_lang` staticlib pinning (`runtime-manifest.json` / `.sha256`, exit `4`) |

## Out of Scope / By Design

| Reported as | Why it is the documented model |
|---|---|
| "A script read my files or opened a socket" | Scripts run with process authority unless restricted via `--sandbox`, `spid3r.ini`, or `Engine::deny` |
| "An AOT executable ignored `--sandbox`" | Native AOT binaries (`spid3r build` / `yamc` / `spid3r-self` via **YamLLVM**) and `extern fn` (T5 FFI) run with full user authority by design (`docs/FFI.md`) |
| "An infinite loop hangs the process" | Bound untrusted scripts with `--budget N`, `memory.max_steps`, `runtime.wall_clock_limit_ms`, or OS cgroups/ulimits |
| "Session payload is readable" | Web sessions are HMAC-signed, not encrypted; do not store secrets in session cookies |
| "No built-in CSRF middleware" | Known gap listed below — use `SameSite` cookies + Origin/CSRF token checks |

## Hardening Controls

- **Untrusted script execution:** Run inside a separate OS user, container, or VM. `spid3r run --sandbox`, `spid3r exec --sandbox` (`spvm`), `Engine::deny` / `deny_all()`, and `spid3r.ini` `[security]` switches are **application-level policy filters** for script engines (`tree`/`vm`) only, not an OS kernel sandbox; when armed they also block `spid3rinfo()`, `config_set()`, `chdir()`, and database natives (`db_open`/`db_connect`/`db_query`).
- **`spid3r.ini` centralized policy (`45/45` keys across `12` sections):**
  - Capability denials: `security.deny_filesystem`, `security.deny_process`, `security.deny_network`, `security.deny_environment`, `security.basedir`, `security.disable_natives`, `modules.disable` (baked once at startup so scripts cannot weaken security, resource-limit, or frozen keys via `config_set`).
  - Resource limits: `memory.max_steps`, `memory.max_call_depth`, `memory.allocation_cap_mb`, `runtime.wall_clock_limit_ms`, `runtime.memory_limit_mb`, `net.timeout_ms`, `net.max_body`, `net.max_headers`.
  - Fail-closed config pin: set `SPID3R_INI=/path/to/spid3r.ini` so a missing or malformed config refuses execution (exit `1`) instead of falling back to defaults.
- **Native AOT & `extern fn` (T5 FFI):** Executables compiled by `spid3r build` / `yamc` / `spid3r-self` run with full user authority; runtime dynamic loading (`dl_open`/`dl_sym`/`dl_call`) is disabled unless `--allow-dlopen` is passed explicitly at build time. When linking vendored `[ffi]` packages, `spid3r build` / `yamc` re-verify both source and `_native/<target>/` hashes against `spider.lock` before invoking the linker (`--no-auto-ffi` to opt out).
- **Integrity, Zero-Network Compiler & Enterprise `spkg` Supply Chain:** v3 `.matrixc` payloads verify SHA-256 integrity, and `SPID3R_RUNTIME_REQUIRE_PIN=1` pins `spid3r_lang` staticlibs via `runtime-manifest.json` / `.sha256` sidecars (exit `4` on mismatch). Compiler commands (`check`, `compile`, `build`, `fmt`, `doc`) perform zero network access. `spkg` / `spid3r install` executes **zero** install-time build scripts, supports `--frozen`/`--offline`, verifies signed metadata (`sha256*.txt.sig`, `manifest*.sha256.sig`, `registry.toml.sig`, `index.txt.sig`, `yanked.txt.sig`, `advisories.txt.sig`) when `packages.trust_pk` is set, restricts `SPID3R_REGISTRY_TOKEN` Bearer auth to same-origin `https://`, checks advisories/yanks via `spkg audit`, and exports CycloneDX 1.5 JSON SBOMs (`spkg freeze --sbom`).
- **Transport TLS & CSPRNG:** Encrypted transport (`https://`, `tls_listen`/`tls_accept`) uses OS-backed `native-tls` behind `--features tls` (never hand-rolled TLS). Use `random_bytes(n)` (OS CSPRNG) for security tokens — not clock-seeded `random_int`/`random_float`.
- **Unsafe code boundary & overflow safety:** `unsafe` blocks are confined to the **14** audited FFI/host-shim modules (`src/native/{ir,link,llvm,rt}.rs`, `src/repl/mod.rs`, `src/runtime_info/mod.rs`, `src/stdlib/{db,entropy,eventloop,file,net,shutdown}.rs`, `src/value/mod.rs`, `src/value/collections/seq.rs`), enforced by `tools/ci/check_unsafe_safety.py` in CI (written `// SAFETY:` justification required on every block), with `overflow-checks = true` enabled in `[profile.release]`.
- **Release & installer verification:** Verify `.sha256` checksums on release tarballs, Windows NSIS setup (`Spid3r-Setup-<ver>.exe`), and Microsoft Store packages (`Spid3r-<ver>-x64.msix`, Store ID `9NPH5XX0F6KF`).
- **Embedding in Rust:** Start from `Engine::new().deny_all()` and grant only the specific capabilities the script requires.

## Known Gaps

| Gap | Status |
|---|---|
| Built-in pure-Rust hashing, KDF, and Ed25519 primitives pass RFC and negative vectors but have **not yet undergone an independent third-party cryptographic audit** (`R12.1`); `jwt_verify` accepts `HS256` only | Open (`R12.1`) |
| No per-host network allowlist (`security.basedir` provides component-wise path scoping with an inherent OS TOCTOU window) | Open |
| Native LLVM AOT backend less battle-tested on hostile untrusted source than tree/VM | Beta for hostile input |
| GUI/webview backend (`wry`/`tao`) smoke-tested only | Open |
| No built-in CSRF middleware in `stdlib/web.matrix` | Open |
| Web sessions are client-readable (HMAC-signed, not encrypted) | By design |
| Hand-written `--embed` launchers keep the lenient payload fallback | Use `run_payload_strict()` |
| Embedded `--embed` payload trailer supports optional Ed25519 signing (`SPID3R_PAYLOAD_SIGN_KEY` + `SPID3R_PAYLOAD_TRUST_PK` via v2 trailer `SPID3R-PAYLOAD02`); unsigned v1 trailers remain accepted when `SPID3R_PAYLOAD_TRUST_PK` is unset | Documented (2026-11 audit M1 / A-10) |
| Under `--sandbox` / `security.deny_filesystem`, `import` skips CWD fallback and non-`.matrix`/`.mt` bare files, and diagnostics suppress quoting source lines from disk; `security.basedir` confines disk imports while keeping embedded stdlib available | Documented (2026-11 audit F2 / A-2) |
| Display, structural equality, and deep-clone walks are depth-capped at 64 (elision marker / `false` / shared tail past depth 64; iterative `Drop` covers all containers including tuples, options, and results) | Documented (2026-11 audit V3 / A-6) |
| `gunzip`/`inflate`/`unzlib` cap output at `min(allocation_cap_mb / memory_limit_mb, 256 MiB)` per call; `zip_read` caps total inflated size at `min(1 GiB, 4 × per-call cap)` | Documented (2026-11 audit D2 / A-7) |
| `tls_listen` cert paths and `http_fetch` `ca_cert` paths are gated by both `security.deny_filesystem` / `--sandbox` and `security.basedir` | Documented (2026-11 audit F5 / A-9) |
| Project-local `spid3r.ini` in the current working directory is ignored by design unless pinned via `SPID3R_INI`; unknown `[security]` keys and case-variant typos warn on stderr | Documented (2026-11 audit A-11 / A-12) |

## Deeper Reading

| Document | Scope |
|---|---|
| [docs/SECURITY.md](docs/SECURITY.md) | Trust model, capability matrix, concurrency, crypto, packaged apps |
| [docs/FFI.md](docs/FFI.md) | Native AOT (`T5`) trust model, POD structs, callbacks & `--allow-dlopen` |
| [docs/selfhost/RUNTIME_ABI.md](docs/selfhost/RUNTIME_ABI.md) | Versioned runtime staticlib manifest & SHA-256 exit-4 contract (`SEC-B`) |
| [docs/AUDIT_REPORT.md](docs/AUDIT_REPORT.md) | Security audit findings and fixes |
| [docs/AUDIT_STATUS.md](docs/AUDIT_STATUS.md) | Open audit items |
