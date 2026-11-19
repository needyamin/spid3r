# Security Policy

**A Spid3r script is code, not data.** By default, a `.matrix` program runs with the full authority of the user process that starts it — it can read files, open sockets, spawn child processes, and read environment variables, just like `sh` or `python`.

![Trust boundary: no gate by default, opt-in capability gate via --sandbox, spid3r.ini, or Engine::deny](docs/diagrams/security.svg)

The primary rule: **if you did not write or audit a script, treat it like a native executable from the same source.**

## Supported Versions

| Version | Supported |
|---|---|
| `1.1.x` | Yes |
| `1.0.x` | No |
| `< 1.0` | No |

## Reporting a Vulnerability

**Do not open a public GitHub issue.** Report security vulnerabilities privately via GitHub Security Advisories:

→ **https://github.com/needyamin/spid3r/security/advisories/new**

Alternatively, email security reports directly to **needyamin@gmail.com** with subject `[SECURITY] Spid3r vulnerability report`.

Please include:
- `spid3r --version` output and your OS/architecture
- The smallest `.matrix` program, `.matrixc` file, `spid3r.ini`, or protocol frame that reproduces the issue
- Expected vs. actual behavior

Response targets: acknowledge within **7 days**; ship a fix or mitigation for critical issues within **30 days**.

## In Scope

| Class | Examples |
|---|---|
| Memory safety | Panic or UB in the Rust host, VM, tree interpreter, or runtime reachable from a `.matrix` program |
| Capability escape | A native or plugin denied via `--sandbox`, `spid3r.ini` (`security.deny_*`, `security.disable_natives`, `modules.disable`, `modules.allow_wasm`/`allow_host`), or `Engine::deny` still reachable from a script |
| Configuration pin bypass | Execution proceeding when an explicit `SPID3R_INI` pin is missing, unreadable, or malformed |
| Remote management (`spid3r-agent/1`) | Authentication/pairing bypass, replay attack, role escalation, remote mutation of schema-frozen keys, or arbitrary command execution |
| Hostile input | Malformed `.matrixc`, corrupt `--embed` payload trailer, hostile JSON/TOML/YAML/XML/regex/WebSocket frames |
| Path traversal | `bundle`, `pack`, `unpack`, `install`, or `security.basedir` escaping its target directory |
| Crypto misuse | ChaCha20+HMAC-SHA256 framing, Ed25519 signature verification, bcrypt, PBKDF2, Argon2, session signing |
| Supply chain & runtime ABI | Bypassing `spider.lock` SHA-256 verification or `spid3r_lang` runtime staticlib pinning (`runtime-manifest.json` / `.sha256`, exit `4`) |

## Out of Scope / By Design

| Reported as | Why it is the documented model |
|---|---|
| "A script read my files or opened a socket" | Scripts run with process authority unless restricted via `--sandbox`, `spid3r.ini`, or `Engine::deny` |
| "An AOT executable ignored `--sandbox`" | AOT binaries (`spid3r build` / `spid3r-self`) and `extern fn` (T5 FFI) run with full user authority by design (`docs/FFI.md`) |
| "An infinite loop hangs the process" | Bound untrusted scripts with `--budget N`, `memory.max_steps`, `runtime.wall_clock_limit_ms`, or OS cgroups/ulimits |
| "Session payload is readable" | Web sessions are HMAC-signed, not encrypted; do not store secrets in session cookies |
| "No built-in CSRF middleware" | Known gap listed below — use `SameSite` cookies + Origin/CSRF token checks |

## Hardening Controls

- **Untrusted script execution:** Run inside a separate OS user, container, or OS sandbox. `spid3r run --sandbox`, `spid3r exec --sandbox`, `Engine::deny`, and `spid3r.ini` `[security]` switches are **application-level policy filters**, not an OS kernel sandbox.
- **`spid3r.ini` centralized policy (60/60 keys enforced):**
  - Capability denials: `security.deny_filesystem`, `security.deny_process`, `security.deny_network`, `security.deny_environment`, `security.basedir`, `security.disable_natives`, `modules.disable`, `modules.allow_wasm`, `modules.allow_host` (baked once at startup so scripts cannot widen their own permissions mid-run via `config_set`).
  - Resource limits: `memory.max_steps`, `memory.max_call_depth`, `memory.allocation_cap_mb`, `runtime.wall_clock_limit_ms`, `runtime.memory_limit_mb`, `net.timeout_ms`, `net.max_body`, `net.max_headers`.
  - Fail-closed config pin: set `SPID3R_INI=/path/to/spid3r.ini` so a missing or malformed config refuses execution (exit `1`) instead of falling back to defaults.
- **Remote management (`spid3r-agent/1` & `spid3r-control.exe`):**
  - All data frames use ChaCha20 + HMAC-SHA256 (encrypt-then-MAC) with per-frame nonces and a server-wide replay window.
  - Pairing requires a one-time 48-hex code with a short TTL (`agent.pairing_ttl_sec`).
  - **No unrestricted remote shell:** every call is checked against the caller's role (`viewer`, `operator`, `admin`) and logged to the append-only `audit.log`.
  - Schema-frozen keys (`security.*`, `logging.*`, `paths.*`, `cache.*`, `modules.allow_*`, `modules.disable`, `packages.allow_*`, `packages.trust_pk`, `agent.enabled/bind/port/limits/roles`) can only be changed on the host (`spid3r.ini` / local `spid3r-control`), never via remote `config.set`.
- **Native AOT & `extern fn` (T5 FFI):** Executables compiled by `spid3r build` or `spid3r-self` run with full user authority; `--sandbox` applies to script engines (`tree`/`vm`) only.
- **Runtime staticlib pinning (`SEC-B`):** Set `SPID3R_RUNTIME_REQUIRE_PIN=1` with `runtime-manifest.json` or `.sha256` sidecars in CI/release builds; tampered or unpinned `spid3r_lang` staticlibs fail closed with exit `4` before linking.
- **Unsafe code boundary:** `unsafe` is strictly confined to `src/native/rt.rs` (AOT FFI trampolines), Linux-only `src/native/sys.rs` (gated behind `#[cfg(all(target_os = "linux", feature = "container"))]`), and bare-metal `kernel/` (`x86_64-unknown-none`), each requiring a written `// SAFETY:` justification.
- **Embedding in Rust:** Start from `Engine::new().deny_all()` and grant only the specific capabilities the script requires.

## Known Gaps

| Gap | Status |
|---|---|
| No per-host network allowlist (`security.basedir` provides component-wise path scoping with an inherent OS TOCTOU window) | Open |
| Native LLVM AOT backend less battle-tested on hostile untrusted source than tree/VM | Beta for hostile input |
| GUI/webview backend (`wry`/`tao`) smoke-tested only | Open |
| No built-in CSRF middleware in `stdlib/web.matrix` | Open |
| Web sessions are client-readable (HMAC-signed, not encrypted) | By design |
| Hand-written `--embed` launchers keep the lenient payload fallback | Use `run_payload_strict()` |

## Deeper Reading

| Document | Scope |
|---|---|
| [docs/SECURITY.md](docs/SECURITY.md) | Trust model, capability matrix, concurrency, crypto, packaged apps |
| [docs/AGENT.md](docs/AGENT.md) | `spid3r.ini` precedence, `spid3r-agent/1` protocol, roles, frozen keys, audit log |
| [docs/selfhost/RUNTIME_ABI.md](docs/selfhost/RUNTIME_ABI.md) | Versioned runtime staticlib manifest & SHA-256 exit-4 contract (`SEC-B`) |
| [docs/AUDIT_REPORT.md](docs/AUDIT_REPORT.md) | Security audit findings and fixes |
| [docs/AUDIT_STATUS.md](docs/AUDIT_STATUS.md) | Open audit items |
