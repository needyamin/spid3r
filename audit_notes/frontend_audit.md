# Front-End Audit Findings (Spid3r)

Source: front-end auditor (lexer/parser/semantic/fmt/REPL/diagnostics), 2026-10-19. Probes in audit_notes/probes/. Report file could not be written by the auditor (tool budget); findings preserved here verbatim from its final answer.

1. **P2 — Type annotations are static-only and bypassable.** `fn f(x: int)` then `let g = f; g("hello")` runs on both engines and prints 42 (probe p02); a nested `fn h(a: int)` is also unchecked (p03b). `src/semantic/ty.rs:1-9` states the checker is compile-time-only. Impact: "gradual typing" is advisory once a function value is rebound or declared locally; documented behavior overstates enforcement. Fix: either enforce argument/return types at call time in both engines (values carry Ty tags) or document precisely which annotations are enforced (direct calls only).
2. **P2 — Privacy hole on non-Ident receivers.** `new C().hide()` on a **private** method runs and prints 1 on both engines; `new C().secret` silently prints null. The semantic checker validates privacy only when the receiver is a bare `Ident` (src/semantic/mod.rs Method/Field arms; probes p05/p05b). Fix: enforce privacy based on the receiver's resolved type, not its syntactic form.
3. **P2 — REPL `:reset` clears only the name list.** `const c=1;` → `:reset` → `let c=2;` → `c=3;` → `error[runtime]: cannot assign to const 'c'` (src/repl/mod.rs:46-50). Const-sets survive reset. Fix: rebuild environment + const sets on :reset.
4. **P3 — REPL bare expressions require `;`.** `2 + 2` errors, contradicting `is_bare_expr`'s "semicolon optional" echo behavior.
5. **P3 — Import bindings use raw `scope.insert` (src/semantic/mod.rs:541).** A same-named local `fn helper` silently shadows an imported binding with no diagnostic (probe p09).

Health: front end is solid — spans are accurate and 1-based, the parse-depth guard works, fmt invariants (re-lex equality + idempotence) hold repo-wide, docs mostly match the code. The gaps are enforcement *completeness* (types/privacy at indirect access paths) and REPL state handling, not happy-path correctness.
