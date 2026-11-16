# Security policy

**A Spid3r script is code, not data.** A `.matrix` program runs with the full authority of the process that starts it — it can read files, open sockets, spawn children and read the environment, exactly like `sh`.

![Trust boundary: no gate by default, opt-in capability gate via --sandbox or Engine::deny](docs/diagrams/security.svg)

The only rule that matters: **if you did not write the script, treat it like a native binary from the same source.**

## Supported versions

| Version | Supported |
|---|---|
| 1.1.x | yes |
| 1.0.x | no |
| < 1.0 | no |

## Report a vulnerability

**Do not open a public issue.** Please report security issues responsibly through GitHub's private vulnerability reporting:

→ **https://github.com/needyamin/spid3r/security/advisories/new**

Alternatively, you can email security reports directly to: **needyamin@gmail.com** (subject: `[SECURITY] Spid3r vulnerability report`).

Include:

- `spid3r version` output and your platform
- the smallest program that reproduces it
- what you expected vs. what happened

Targets: acknowledge within **7 days**, ship a fix or mitigation for critical issues within **30 days**.

## In scope

| Class | Example |
|---|---|
| Memory safety | panic or UB in the toolchain reachable from a `.matrix` program |
| Capability escape | a `deny`-ed native still reachable from an embedded script |
| Hostile input | malformed `.matrixc`, corrupt payload trailer, hostile TOML/YAML/XML/JSON |
| Path traversal | `bundle`, `pack`, `unpack`, `install` writing outside the target |
| Crypto misuse | bcrypt, HMAC, PBKDF2, Argon2, session signing |
| Supply chain & runtime ABI | a `spider.lock` pin or `spid3r_lang` runtime SHA-256 pin (`runtime-manifest.json` / `.sha256`, exit `4` `SEC-B`) that can be bypassed |

## Not a vulnerability

| Reported as | Why it is the documented model |
|---|---|
| "a script read my files" | scripts have process authority by design — see the diagram |
| "an infinite loop hangs the process" | cap it with `--budget N` or OS limits |
| "session payload is readable" | sessions are signed, not encrypted; don't store secrets |
| "no CSRF middleware" | known gap, listed below — add one or use `SameSite` + origin check |

## Hardening

- **Running untrusted code:** separate user, container, or OS sandbox. `spid3r run --sandbox` and `Engine::deny` filter script-engine natives (`tree`/`vm`) and are policy filters, not an OS sandbox.
- **Native AOT & `extern fn` scope:** executables produced by `spid3r build` or `spid3r-self` (and T5 `extern fn` C FFI) always run with full user authority; `--sandbox` applies to script engines only.
- **Runtime staticlib pinning (`SEC-B`):** set `SPID3R_RUNTIME_REQUIRE_PIN=1` with `runtime-manifest.json` or `.sha256` sidecars in CI/release builds; tampered or unpinned `spid3r_lang` staticlibs fail closed with exit `4` before linking.
- **Embedding in Rust:** `Engine::new().deny_all()`, then grant only what the script needs.
- **Bounding work:** `spid3r run --budget N` caps execution for untrusted input.

## Known gaps

| Gap | Status |
|---|---|
| No per-host network allowlist or memory ceiling (`security.basedir` provides approximate path scope with a TOCTOU window) | open |
| Native (LLVM) backend less exercised than tree/VM | beta for hostile input |
| GUI/webview backend smoke-tested only | open |
| No CSRF middleware in the web stack | open |
| Sessions are client-readable (signed, not encrypted) | by design |
| Hand-written `--embed` launchers keep the lenient payload fallback | call `run_payload_strict()` |

## Deeper reading

| Document | |
|---|---|
| [Security model](docs/SECURITY.md) | trust model, concurrency rules, crypto, packaged apps |
| [Selfhost Runtime ABI](docs/selfhost/RUNTIME_ABI.md) | versioned runtime staticlib manifest & SHA-256 exit-4 verification contract (`SEC-B`) |
| [Audit report](docs/AUDIT_REPORT.md) | findings and their fixes |
| [Audit status](docs/AUDIT_STATUS.md) | what is still open |
