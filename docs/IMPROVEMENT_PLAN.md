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
2. ~~**Default parameter values**~~ — **done 2026-10-22** (v1 scope:
   **constant defaults**, trailing-only, parser-enforced; expression defaults
   referencing earlier parameters are future work). Landed across ALL THREE
   engines: parser + `Param.default` + `FnProto::min_arity`; semantic arity
   checks became ranges (6 sites, message format stable when min==max);
   interpreter fills from the default exprs; VM bakes default VALUES into
   `VmFuncProto`/`VmFunc` and fills at `call_vfunc_frame` (every call path
   funnels there); native embeds the constants in the generated prologue
   (argc-conditional phi) and the dispatch FFI gained `min_arity`.
   Bytecode format bumped to **v2** per docs/BYTECODE_STABILITY.md (reader
   dual-decodes v1+v2; writer emits v2). `default_params` matrix case with
   `aot: true`; count 696 -> 702.
3. ~~**Named arguments**~~ — **done 2026-10-22**: `f(x: 1, y: 2)` on direct
   function calls, constructors, and `super` (parser: lookahead for
   `ident :`; `Expr::NamedArg`). The checker validates names/duplicates/
   ordering/coverage against the callee signature (FuncInfo/MethodInfo gained
   `param_names`); `lower` — the desugar seam — reorders into positional form
   and substitutes each omitted parameter's own constant default into its
   slot (so `new Point(y: 5)` binds correctly rather than shifting). Engines
   never see the node (defensive internal-error arms everywhere). Documented
   v1 limits: method calls and calls through function values reject named
   arguments with precise diagnostics. 4 matrix rows (one aot: true).
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
6. **stdlib growth in pure Spid3r & native additions** (libraries
   written in Spid3r, shipped in `stdlib/`):
   - ~~**assertions framework**~~ (**done** — `stdlib/assert.matrix`).
   - ~~**TOML parser**~~ (**done** — `stdlib/toml.matrix`).
   - ~~**YAML parser**~~ (**done** — `stdlib/yaml.matrix`).
   - ~~**XML parser & writer**~~ (**done** — `stdlib/xml.matrix`).
   - ~~**SemVer 2.0.0**~~ (**done** — `stdlib/semver.matrix`).
   - ~~**UUID v4**~~ (**done** — `stdlib/uuid.matrix`).
   - ~~**Codecs**~~ (**done** — `base32_encode/decode`, `hex_encode/decode`).
   - ~~**BigInt, Decimal, Rational, Complex**~~ (**done** — `bigint.matrix`, `decimal.matrix`, `rational.matrix`, `complex.matrix` with operator overloading and literal syntax).
   - ~~**Generators & yield**~~ (**done** — `stdlib/gen.matrix` and `yield` lowering).
   - ~~**Event Loop**~~ (**done** — `stdlib/event.matrix` over `tcp_poll`).
   - ~~**Timezone Database**~~ (**done** — embedded tzdata 2026e, 343 zones, `stdlib/tz.matrix`).
   - ~~**Zero-Dependency Database Drivers**~~ (**done** — `mysql_*`, `pg_*`, `redis_*`).
   - ~~**Media Formats**~~ (**done** — `png_encode/decode`, `wav_encode/decode`, `pdf_write`).

## DESIGN — ongoing ecosystem work

- **Remote package registry + signing** (PKG-01..04): server protocol,
  Ed25519 signing, content-addressed store. Multi-session ecosystem work —
  the highest *strategic* value item in this document.
- **Generics, macros, reflection** (LANG-02/06/07):
  language-design projects; sequence after the registry so real
  users shape the design.

## Explicitly NOT planned (rejected, with reasons)

- **Arbitrary i64 overflow → error**: would break the VM's hot loop contract
  (every add pays a check); the languages we compare against either wrap or
  go bignum. The fix is BigInt for the cases that need it, not slowing down
  every increment. Documented behavior.
- **Byte strings for indexing**: the char-indexed model is *more* correct
  than PHP's; keep it.
