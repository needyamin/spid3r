# Spid3r improvement plan — closing the verified gaps

Status: live planning doc (2026-10-22). Feeds from the measured comparison
(`comparison_python_php.html`): every entry below is a verified gap, not a
guess. Phases: **NOW** (self-contained, implementable immediately) · **NEXT**
(designed, roughly one focused session each) · **DESIGN** (needs a design
round or ecosystem build-out). Owned items reference the architect report's
issue IDs (report parked in `_dustbin/architect_report.txt`).

## NOW — executed 2026-10-22

| Gap (verified) | Fix | Status |
| --- | --- | --- |
| No password hashing (bcrypt/argon2 class) | `pbkdf2_sha256(password, salt, iterations)` native — PBKDF2-HMAC-SHA256 over the existing `hmac_sha256_bytes`, RFC 8018, tested against published vectors | **done** |
| Base64 only as bytes methods (no string-level/URL-safe forms) | `base64_encode/decode` + `base64_url_encode/decode` natives (RFC 4648, tested against its vectors). Note: `bytes.to_base64/from_base64/to_hex/from_hex` already existed — the comparison report was corrected, not all of base64 was missing | **done** |
| No UDP | `udp_bind / udp_local_addr / udp_send_to / udp_recv_from / udp_close` natives following the `tcp_*` handle pattern; real loopback test | **done** |

## NEXT — designed, one focused session each (in impact order)

1. ~~**HTTPS/TLS client**~~ — **done 2026-10-22** (this round): opt-in
   `tls` feature; `http_fetch` speaks https:// via native-tls (OS certificate
   store), verified against the live web; framing refactored to be
   transport-generic; core stays zero-dependency without the feature.
   Remaining sub-item: a raw `tls_connect(host, port)` handle native when a
   user needs non-HTTP TLS.
2. **Default parameter values** (report PAR-*). Design sketch: `FnProto`
   gains `defaults: Vec<Option<Expr>>`; the arity check becomes a range
   (`argc < min_arity` errors); each engine's callee prologue evaluates
   missing trailing defaults *inside the callee* (earlier params are visible,
   Python-style). Touches: parser, `FnProto`, semantic (arity range), VM
   compiler prologue + runtime, interpreter call path, native prologue,
   docs, matrix rows. Effort: L, three engines — needs a session of its own.
3. **Named arguments** — parse `f(x: 1)`; checker binds by name against the
   prototype; engines normalize call order before invocation. Effort: M/L.
4. ~~**LSP rename + find-references**~~ — **done 2026-10-22** (this round):
   `textDocument/references` + `textDocument/rename` over the existing
   declaration index, file-scoped, shadow-aware; nested-block extents were
   narrowed per block so shadowing is modeled correctly. 6 wire tests added
   (23 LSP tests total). Remaining sub-items: cross-file references (needs
   the import graph), signatureHelp, codeAction.
5. ~~**Profiler**~~ — **done 2026-10-22** (this round): `spid3r run --prof`
   (vm) counts every executed instruction by opcode — one array increment per
   instruction, zero cost when off — and prints a sorted per-opcode report
   (counts + share) to stderr after the run. 3 wire tests, including a
   counts-sum-to-total assertion. Remaining sub-item: per-function self-time
   attribution (needs call/return instrumentation) for flamegraph-style data.
6. **stdlib growth in pure Spid3r** (the web.matrix pattern — libraries
   written in Spid3r, shipped in `stdlib/`): TOML subset parser, CSV→dataset
   helpers, hex/base32, semver, uuid v4 (from `random`), a tiny test
   assertions framework (`assert.sp`) to fix `spid3r test` stdout-only
   weakness. Each: S/M, no compiler changes.

## DESIGN — needs a design round or sustained ecosystem work

- **Evented I/O / async runtime** (CONC-01): event loop + non-blocking
  sockets + `async/await` syntax; the scheduler already parks threads, so the
  runtime seam exists, but this is a multi-session project.
- **Generators** (`yield`): needs coroutine frames in the VM (separate value
  stack + resume state) across all engines. Multi-session.
- **BigInt / Decimal**: arbitrary-precision arithmetic as a native module
  (or a carefully-specified string-based stdlib lib). Design decision:
  core-native (fast, zero-dep) vs stdlib-Spid3r (slow, safe).
- **Timezone database**: needs shipping tzdata; decide embedding strategy.
- **Remote package registry + signing** (PKG-01..04): server protocol,
  Ed25519 signing, content-addressed store. Multi-session ecosystem work —
  the highest *strategic* value item in this document.
- **Generics, operator overloading, macros, reflection** (LANG-02/05/06/07):
  each is a language-design project; sequence after the registry so real
  users shape the design.
- **MySQL/Postgres drivers**: protocol clients over TCP; blocked on TLS for
  auth (caching_sha2/mysql_native_password need TLS or careful hashing).
- **Image/media processing**: realistically an FFI/extension story, not
  stdlib — needs the extension ABI design (ARC-09).

## Explicitly NOT planned (rejected, with reasons)

- **Arbitrary i64 overflow → error**: would break the VM's hot loop contract
  (every add pays a check); the languages we compare against either wrap or
  go bignum. The fix is BigInt for the cases that need it, not slowing down
  every increment. Documented behavior.
- **Byte strings for indexing**: the char-indexed model is *more* correct
  than PHP's; keep it.
