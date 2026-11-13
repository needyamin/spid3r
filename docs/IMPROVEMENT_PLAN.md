# Spid3r improvement plan — closing the verified gaps

Status: live planning doc (2026-10-22, updated 2026-10-29). Feeds from the measured benchmark comparison:
every entry below is a verified gap, not a guess. Phases: **NOW** (self-contained, implementable immediately) · **NEXT**
(designed, roughly one focused session each) · **DESIGN** (needs a design
round or ecosystem build-out). Owned items reference the architect report's
issue IDs (documented in `docs/AUDIT_REPORT.md` and `docs/ROADMAP.md`).

## 2026-10-29 — filesystem completeness + execution budget

| Gap (verified) | Fix | Status |
| --- | --- | --- |
| No directory listing / stat / process context (could not walk a directory or read a file's size — the #1 real-app blocker vs Python/Java) | `list_dir` (sorted, no `.`/`..`), `is_dir`, `is_file`, `file_size`, `file_mtime`, `cwd`, `chdir`, `env_set` natives; `Environment` capability now covers `env_set`, `FileSystem` covers the new stat/list/chdir natives | **done** |
| No lexical path helpers (users hand-rolled separator handling, breaking on Windows) | `path_join`, `basename`, `dirname`, `path_ext` — pure string natives; both `/` and `\` are separators on every OS, results normalize to `/` | **done** |
| `yaml_parse` silently misparsed unbalanced flow collections (`"a: [1, 2"` → string) | `flow_close_index` rejects unterminated/mismatched/trailing-text collections with a catchable error; 120-case differential byte-identical on valid input | **done** |
| No execution budget (untrusted scripts could loop forever — documented hole in SECURITY.md) | `Budget` (shared saturating counter); tree engine charges per statement + per loop iteration, VM charges per instruction poll; exhaustion is a **fatal** error that `try`/`catch` cannot intercept; shared with spawned tasks and module init; `Engine::budget(n)` + `spid3r run --budget N` | **done** |

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
   the import graph) and signatureHelp; codeAction shipped since then
   (`textDocument/codeAction`, quick fixes derived from our own diagnostics).
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
  than byte-indexed alternatives; keep it.

## 2026-11-13 — tree-engine slot resolution (W4.1) and the int fast path (W4.2): measured, not built

The optimize plan projected a ≥2.5× "tree compute" win from slot-resolved
variables plus an int/int fast path. Fresh measurement refutes the
projection, and both items are **rejected** on that evidence:

| Measurement (tree engine, 2M-iteration loop, release build, median of 3) | Result |
| --- | --- |
| `while i < n { i = i + 1; }` (dispatch floor: condition eval + 1 read + 1 assign) | 237.8 ms — 119 ns/iteration |
| `while i < n { total = total + i; i = i + 1; }` (the arith shape) | 382.7 ms — 191 ns/iteration |

Even a **zero-cost** variable system leaves the dispatch floor at
237.8 ms, so slot resolution alone caps the arith win at ~1.6× — the
≥2.5× gate is arithmetically unreachable without redesigning the eval
dispatch itself (the interpreter's `eval` recursion, `Flow` plumbing and
per-statement budget charging), which is a different and much larger
project than the plan described.

What the measurement also showed:

- The int/int fast path **already exists**: `eval_binary`'s first match
  arm handles `Int + Int` directly through `checked_int`. There is no
  remaining int fast-path win of substance.
- The assign path **already reads its target exactly once**: `Expr::Assign`
  evaluates the value first and the plain-`=` arm assigns without
  re-reading the target, so the double-read the VM's fusion eliminates
  does not exist in the tree interpreter.

Cost/benefit of the rejected W4.1 refactor: annotating the AST with
resolved scopes, converting `EnvironmentData` to slot arrays, and
re-plumbing every named access (consts, module imports, REPL defines,
the debugger, LSP, task-shared envs) is an L-size change to the
**semantic reference engine** for a measured ~1.4–1.6× win on one
engine's microbenchmarks — while the VM is already ~9× faster on the
same workload and `D4.1` (VM as the default `run` engine, blocked on
owner approval) addresses the tree's absolute speed directly. A defect
in the oracle engine poisons every differential gate downstream.

Recorded so the next planner does not re-project these numbers.
