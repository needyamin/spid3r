# Market & Competitive Analysis (Spid3r)

Source: market analyst (web-enabled), 2026-10-19. Verbatim from its final answer; citations are the analyst's.

## 0. Grounding & contradictions (verified in repo)
sitemap.txt: 308 tests green, 23/23 three-engine differential, v1.0.0, zero-dep core. `src/stdlib/net.rs:41–52` implements `tcp_listen`…`http_fetch`; **contradiction:** sitemap §6 says "no HTTP beyond 4 net tests" — the code says otherwise; sitemap §6 is stale, README matches code. Real residual gap: no TLS ("TLS would need a third-party crate", net.rs:702). Second contradiction: the only performance data is self-run (benchmarks.html, vs Python 3.15.0 / PHP 8.3.33) — treat perf claims as same-class hypotheses until independently rerun.

## 1. Comparison table
(perf column: high-confidence judgment + sources; rest: general knowledge unless flagged)

| Dim | Spid3r | Python | JS | Lua | Ruby | PHP | Rust |
|---|---|---|---|---|---|---|---|
| Typing | dynamic + pre-run checked annotations | dynamic (mypy) | dynamic (TS separate) | dynamic (Teal) | dynamic (Sorbet) | dynamic | static, affine |
| Runtime/perf | tree ≈ unspecialized CPython; VM ≈ PUC-Lua/CPython class (self-bench: 1.28–1.92× vs Py 3.15, 8 micro-workloads); LLVM AOT on a subset | 3.13+ JIT still experimental/off-by-default ([PEP 836](https://peps.python.org/pep-0836)) | V8, top JIT class | PUC-Lua ≫ CPython; LuaJIT near-C ([HN](https://news.ycombinator.com/item?id=18889788)) | YJIT prod, 57% over 3.1.3 ([Shopify](https://shopify.engineering/ruby-yjit-is-production-ready)) | 8.x JIT, web-tuned | AOT, near-C |
| Memory | RC + trial-deletion cycle GC | refcount+cycle GC, GIL | tracing GC | incremental GC | GC | GC | ownership |
| Errors/debug | spans+trace, line debugger, REPL | best tracebacks | best tooling | minimal | decent | decent | hardest write, best catch |
| Stdlib | small: 20+ collections, tasks/channels, JSON, TCP/HTTP no-TLS, no regex/date-time | huge | platform-based | tiny by design | medium | huge web | tiny core + crates |
| Ecosystem | none (local/git/dir) | ~890k packages ([pypistats](https://pypistats.org/)) | ~3.1M ([npm](https://en.wikipedia.org/wiki/Npm)) | medium | ~180k gems | large | ~180k crates |
| Distribution | single static exe, NSIS, WASM, bundler | runtime install | Node runtime | ~300KB embeddable | runtime | runtime+server | static exe |
| Tooling | fmt/REPL/debug/difftest in one binary; **no LSP** | mature | best | minimal | mature | mature | cargo/rust-analyzer |
| Curve | small C-like grammar | easy | easy | tiny | medium | easy | steep |

Janet/Wren occupy the same embed niche smaller; Nim/Crystal are compiled alternatives — not Spid3r's fight.

## 2. Honest gaps (don't attempt)
Ecosystem: decades behind; unfixable by code, only by years of users. Perf-at-scale: no specializing JIT; being 1.3× over CPython on cherry-picked micro-workloads is same-class evidence, not LuaJIT-class. TLS/regex/date-time/FFI absent → real-world scripting hits walls fast. LSP: table stakes for any dynamic language.

## 3. Genuine advantages (verified capabilities)
(i) Zero-dep pure-Rust core → one auditable static binary, trivial supply chain — neither Lua (C) nor CPython embeds cleanly into Rust; in-Rust competitors are Rhai (expression-oriented, no classes/modules) and Rune — Spid3r is broader-surface than both. (ii) Dual differential-tested engines + fuzzing + optimizer is rare rigor — a living "Crafting Interpreters". (iii) 20+ built-in collections incl. trees/graphs with cycle-safe GC → teach data structures import-free. (iv) tasks/channels built in. (v) WASM playground with zero deps. (vi) Windows NSIS-first care is genuinely uncommon.

## 4. Positioning recommendation
Pick **(a) education/teaching artifact** + **(c) embedded scripting for Rust apps**, with (e) portfolio free-riding. (a) needs no ecosystem; the codebase is the curriculum (front end → two engines → differential harness → optimizer → AOT). (c) structurally matches Lua's niche; precondition: ship a documented, stable library/embedding API — today the crate is CLI-first, so this is the one build that unlocks the niche. Reject (b) Windows admin scripting: PowerShell owns it; no regex/date-time/TLS is disqualifying. Reject (d) hobbyist community as primary: needs LSP + packages first. Failure signal for (c): no third-party embed within ~6 months of shipping the API.

## 5. Adoption barriers (ranked)
1) No LSP/editor support. 2) No packages to import. 3) Stdlib holes (regex, date-time, TLS). 4) No FFI escape hatch — "can't do X" is terminal. 5) One-author bus factor, no independent perf evidence.
**Don't build now:** specializing JIT; remote registry/discovery infra (chicken-egg); C FFI (kills the zero-dep story); full static-typing expansion (2× maintenance across engines); Android/iOS polish; self-hosting.

## 6. Path
OSS-first; KPIs: GitHub stars, playground sessions, then lib downloads once the embed API ships. Publish a third-party-rerunnable benchmark methodology. Commercialization of a v1.0 language is unrealistic — closest realistic revenue: embedding-integration consulting.

## Missing evidence
(1) Independent perf rerun → would confirm/refute VM≈PUC-Lua class. (2) Embedding-API ergonomics (doesn't exist yet) → decides whether (c) is near-term or ~6 months of work. (3) Gradual-typing checker recall → decides whether "gradual" is substance or marketing.
