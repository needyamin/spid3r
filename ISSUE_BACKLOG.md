# Spid3r / GaliLang — Issue Backlog (GitHub-importable)

This file mirrors `architect_report.txt` §5. Each issue is ready to create via
the `gh` CLI. After `gh auth login`, run `bash tools/create_issues.sh` from the
repo root (or paste the commands below). Titles/bodies are self-contained; the
full rationale lives in the report.

Label legend:
  area:   architecture | performance | concurrency | parity | language |
          stdlib | packaging | tooling | security | ci | docs
  prio:   P0 (high) | P1 (medium) | P2 (low)
  phase:  A | B | C | D | E

--------------------------------------------------------------------------------
## Issue list (ID · title · area · prio · phase)
--------------------------------------------------------------------------------
ARC-01  Promote `Chunk` to the canonical portable IR                       architecture P0 A
ARC-02  Split monolith into a workspace of crates                         architecture P0 A
ARC-03  Move all desugaring out of the parser into `lower`                architecture P1 A
ARC-04  Define and enforce crate public-API boundaries                    architecture P1 A
ARC-05  Make Native/AOT consume `Chunk` instead of the AST               architecture P0 B
ARC-06  Introduce a formal Engine Contract test surface                  architecture P1 B
ARC-07  Adopt a workspace-wide error type (dependency-free)               architecture P2 B
ARC-08  Decouple `sched` from the global singleton for testability        architecture P1 B
ARC-09  Add a stable C/FFI embedding ABI version header                  architecture P1 C
ARC-10  Research self-hosting the compiler in Spid3r                     architecture P2 E

PERF-01 Replace poisoned-mutex GC gate with incremental/generational GC    performance P0 B
PERF-02 Interning of identifiers/field names (symbol table)              performance P1 B
PERF-03 Register-based or threaded-code VM experiment                    performance P1 B
PERF-04 Per-call frame allocation reuse / cache-friendly CallFrame       performance P2 B
PERF-05 Compile-time constant evaluation                                  performance P1 B
PERF-06 Tracing/JIT pilot over `Chunk` for hot loops                     performance P1 D

CONC-01 Async I/O core + event loop                                      concurrency P0 B
CONC-02 TLS support for net/http (feature-gated)                         concurrency P0 B
CONC-03 Work-stealing scheduler option for fine-grained tasks            concurrency P1 B
CONC-04 Per-task resource budgets (CPU/instructions/alloc)               concurrency P1 B
CONC-05 Structured concurrency (task scopes / cancellation)              concurrency P1 D

PAR-01  Native: support classes declared inside functions               parity P0 A
PAR-02  Native: support higher-order collection callbacks               parity P0 A
PAR-03  Native: support `super`                                          parity P1 A
PAR-04  Unify remaining diagnostic phase labels across engines           parity P1 A
PAR-05  Add tuple literals `(1,2,3)`                                     parity P2 A
PAR-06  Add `as` cast syntax                                             parity P2 A
PAR-07  Field type annotations enforced on external `obj.field = v`     parity P1 B
PAR-08  Gradual typing hardening (rebound fn values, nested `fn`)       parity P1 B
PAR-09  Raise/stream VM 255-arg/255-local/128-capture hard limits       parity P1 B
PAR-10  Interpolation holes beyond `{name}`/`{obj.field}`               parity P2 B

LANG-01 Records/structs as a distinct value kind                        language P1 B
LANG-02 Generics / parametric types                                     language P1 C
LANG-03 `async`/`await` language syntax over async runtime               language P1 B
LANG-04 Enum methods + associated functions                             language P2 C
LANG-05 Operator overloading via traits                                language P1 C
LANG-06 Macros / compile-time metaprogramming                           language P1 D
LANG-07 Compile-time reflection / introspection API                     language P2 D
LANG-08 Module namespaces + `import ... as` + `from` imports            language P1 C
LANG-09 Const blocks / compile-time config                             language P2 B

STD-01  Split `stdlib/mod.rs`; namespace stdlib (`std/net`, `std/json`)  stdlib P1 C
STD-02  Stdlib versioning / semver-compatible updates                    stdlib P1 C
STD-03  Expand containers per demand (rope, persistent map)              stdlib P2 C
STD-04  Timezone/IO/encoding (UTF-8 streams, gzip) libraries            stdlib P2 C
STD-05  Math/linear-algebra stdlib module                               stdlib P2 C

PKG-01  Remote (HTTP) package registry protocol + client                packaging P0 C
PKG-02  Package signing (Ed25519) + verification on install             packaging P0 C
PKG-03  Content-addressed storage for packages                          packaging P1 C
PKG-04  Transitive dependency resolution for registry packages          packaging P1 C
PKG-05  `dist` produces self-contained multi-arch native artifacts       packaging P1 C
PKG-06  System-package distribution (deb/rpm/brew)                      packaging P2 D

TOOL-01 Incremental LSP analysis (no full re-parse per keystroke)        tooling P0 C
TOOL-02 LSP: rename + find-references                                   tooling P1 C
TOOL-03 LSP: semantic tokens + signature help                           tooling P1 C
TOOL-04 LSP: code actions + quick-fixes                                 tooling P1 C
TOOL-05 LSP: workspace symbols + multi-file index                       tooling P2 C
TOOL-06 Profiler / flamegraph for tree & VM                             tooling P1 B
TOOL-07 Coverage tooling for the differential suite                     tooling P2 B
TOOL-08 Debugger: conditional breakpoints + watch + REPL-in-frame        tooling P1 D

SEC-01  Resource metering for embed API (CPU/instructions/alloc)         security P0 B
SEC-02  Capability/sandbox descriptor: deny-by-default natives          security P1 B
SEC-03  Replace pervasive `.lock().unwrap()` with non-poisoning guards   security P1 B
SEC-04  Bytecode signing / verification for distributed `.matrixc`      security P1 D
SEC-05  Hardening pass on the async + TLS stack                         security P1 D
SEC-06  SBOM + dependency/feature attestation in releases               security P2 D

CI-01   Perf-budget CI (benchmarks with regression thresholds)           ci P1 A
CI-02   Expand differential matrix to native + wasm engines              ci P1 A
CI-03   Pin/report canonical test counts in CI summary                  ci P2 A
CI-04   Multi-platform native CI (Linux/mac/Windows LLVM builds)         ci P1 D
CI-05   Broaden fuzzing (semantic, optimizer, registry, net framing)     ci P1 D
CI-06   Nightly engine-contract job across all backends                 ci P2 D

DOC-01  Bytecode / `.matrixc` stability & compatibility policy           docs P1 A
DOC-02  Generate reference docs from source annotations                  docs P1 D
DOC-03  Published stability & security policy pages                      docs P2 D
DOC-04  Architecture decision records (ADRs) for IR/crate split          docs P2 A

--------------------------------------------------------------------------------
## create_issues.sh
--------------------------------------------------------------------------------
# Usage: gh auth login && bash tools/create_issues.sh
# Each issue is created with area + prio + phase labels. Bodies are one-liners;
# see architect_report.txt §5 for full rationale.

gh_issue () {
  local id="$1"; local title="$2"; local area="$3"; local prio="$4"; local phase="$5"
  local body="ID: $id | Area: $area | Priority: $prio | Phase: $phase
See architect_report.txt (§5) for full rationale, recommended approach, and exit criteria."
  gh issue create --title "$title" --body "$body" --label "area:$area,prio:$prio,phase:$phase"
}

gh_issue ARC-01  "Promote Chunk to the canonical portable IR"                       architecture P0 A
gh_issue ARC-02  "Split monolith into a workspace of crates"                        architecture P0 A
gh_issue ARC-03  "Move all desugaring out of the parser into lower"                architecture P1 A
gh_issue ARC-04  "Define and enforce crate public-API boundaries"                   architecture P1 A
gh_issue ARC-05  "Make Native/AOT consume Chunk instead of the AST"               architecture P0 B
gh_issue ARC-06  "Introduce a formal Engine Contract test surface"                 architecture P1 B
gh_issue ARC-07  "Adopt a workspace-wide error type (dependency-free)"             architecture P2 B
gh_issue ARC-08  "Decouple sched from the global singleton for testability"        architecture P1 B
gh_issue ARC-09  "Add a stable C/FFI embedding ABI version header"                 architecture P1 C
gh_issue ARC-10  "Research self-hosting the compiler in Spid3r"                     architecture P2 E

gh_issue PERF-01 "Replace poisoned-mutex GC gate with incremental/generational GC"  performance P0 B
gh_issue PERF-02 "Interning of identifiers/field names (symbol table)"             performance P1 B
gh_issue PERF-03 "Register-based or threaded-code VM experiment"                    performance P1 B
gh_issue PERF-04 "Per-call frame allocation reuse / cache-friendly CallFrame"      performance P2 B
gh_issue PERF-05 "Compile-time constant evaluation"                                performance P1 B
gh_issue PERF-06 "Tracing/JIT pilot over Chunk for hot loops"                      performance P1 D

gh_issue CONC-01 "Async I/O core + event loop"                                    concurrency P0 B
gh_issue CONC-02 "TLS support for net/http (feature-gated)"                       concurrency P0 B
gh_issue CONC-03 "Work-stealing scheduler option for fine-grained tasks"          concurrency P1 B
gh_issue CONC-04 "Per-task resource budgets (CPU/instructions/alloc)"             concurrency P1 B
gh_issue CONC-05 "Structured concurrency (task scopes / cancellation)"            concurrency P1 D

gh_issue PAR-01  "Native: support classes declared inside functions"              parity P0 A
gh_issue PAR-02  "Native: support higher-order collection callbacks"              parity P0 A
gh_issue PAR-03  "Native: support super"                                          parity P1 A
gh_issue PAR-04  "Unify remaining diagnostic phase labels across engines"         parity P1 A
gh_issue PAR-05  "Add tuple literals (1,2,3)"                                      parity P2 A
gh_issue PAR-06  "Add as cast syntax"                                              parity P2 A
gh_issue PAR-07  "Field type annotations enforced on external obj.field = v"      parity P1 B
gh_issue PAR-08  "Gradual typing hardening (rebound fn values, nested fn)"        parity P1 B
gh_issue PAR-09  "Raise/stream VM 255-arg/255-local/128-capture hard limits"     parity P1 B
gh_issue PAR-10  "Interpolation holes beyond {name}/{obj.field}"                   parity P2 B

gh_issue LANG-01 "Records/structs as a distinct value kind"                       language P1 B
gh_issue LANG-02 "Generics / parametric types"                                    language P1 C
gh_issue LANG-03 "async/await language syntax over async runtime"                language P1 B
gh_issue LANG-04 "Enum methods + associated functions"                            language P2 C
gh_issue LANG-05 "Operator overloading via traits"                               language P1 C
gh_issue LANG-06 "Macros / compile-time metaprogramming"                         language P1 D
gh_issue LANG-07 "Compile-time reflection / introspection API"                   language P2 D
gh_issue LANG-08 "Module namespaces + import ... as + from imports"              language P1 C
gh_issue LANG-09 "Const blocks / compile-time config"                            language P2 B

gh_issue STD-01  "Split stdlib/mod.rs; namespace stdlib (std/net, std/json)"      stdlib P1 C
gh_issue STD-02  "Stdlib versioning / semver-compatible updates"                  stdlib P1 C
gh_issue STD-03  "Expand containers per demand (rope, persistent map)"            stdlib P2 C
gh_issue STD-04  "Timezone/IO/encoding (UTF-8 streams, gzip) libraries"          stdlib P2 C
gh_issue STD-05  "Math/linear-algebra stdlib module"                             stdlib P2 C

gh_issue PKG-01  "Remote (HTTP) package registry protocol + client"              packaging P0 C
gh_issue PKG-02  "Package signing (Ed25519) + verification on install"           packaging P0 C
gh_issue PKG-03  "Content-addressed storage for packages"                       packaging P1 C
gh_issue PKG-04  "Transitive dependency resolution for registry packages"        packaging P1 C
gh_issue PKG-05  "dist produces self-contained multi-arch native artifacts"       packaging P1 C
gh_issue PKG-06  "System-package distribution (deb/rpm/brew)"                    packaging P2 D

gh_issue TOOL-01 "Incremental LSP analysis (no full re-parse per keystroke)"      tooling P0 C
gh_issue TOOL-02 "LSP: rename + find-references"                                 tooling P1 C
gh_issue TOOL-03 "LSP: semantic tokens + signature help"                        tooling P1 C
gh_issue TOOL-04 "LSP: code actions + quick-fixes"                              tooling P1 C
gh_issue TOOL-05 "LSP: workspace symbols + multi-file index"                    tooling P2 C
gh_issue TOOL-06 "Profiler / flamegraph for tree & VM"                          tooling P1 B
gh_issue TOOL-07 "Coverage tooling for the differential suite"                   tooling P2 B
gh_issue TOOL-08 "Debugger: conditional breakpoints + watch + REPL-in-frame"      tooling P1 D

gh_issue SEC-01  "Resource metering for embed API (CPU/instructions/alloc)"       security P0 B
gh_issue SEC-02  "Capability/sandbox descriptor: deny-by-default natives"        security P1 B
gh_issue SEC-03  "Replace pervasive .lock().unwrap() with non-poisoning guards"   security P1 B
gh_issue SEC-04  "Bytecode signing / verification for distributed .matrixc"      security P1 D
gh_issue SEC-05  "Hardening pass on the async + TLS stack"                       security P1 D
gh_issue SEC-06  "SBOM + dependency/feature attestation in releases"            security P2 D

gh_issue CI-01   "Perf-budget CI (benchmarks with regression thresholds)"        ci P1 A
gh_issue CI-02   "Expand differential matrix to native + wasm engines"           ci P1 A
gh_issue CI-03   "Pin/report canonical test counts in CI summary"               ci P2 A
gh_issue CI-04   "Multi-platform native CI (Linux/mac/Windows LLVM builds)"      ci P1 D
gh_issue CI-05   "Broaden fuzzing (semantic, optimizer, registry, net framing)"  ci P1 D
gh_issue CI-06   "Nightly engine-contract job across all backends"              ci P2 D

gh_issue DOC-01  "Bytecode / .matrixc stability & compatibility policy"          docs P1 A
gh_issue DOC-02  "Generate reference docs from source annotations"               docs P1 D
gh_issue DOC-03  "Published stability & security policy pages"                   docs P2 D
gh_issue DOC-04  "Architecture decision records (ADRs) for IR/crate split"       docs P2 A

echo "Created $(grep -c '^gh_issue ' "$0") issues."
