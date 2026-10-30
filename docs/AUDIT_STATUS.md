# Documentation Audit Status

Durable status of the 2026-10-30 documentation audit.

The original audit report was a one-off HTML snapshot and has been removed; this
file is the live answer to "is issue N still open?". Every row carries a command
or a file:line you can check in this repository, so nothing here depends on the
audit's local scratch tree (which has also been removed — it was gitignored and
fully regenerable).

**Last verified:** 2026-10-31 against `fb1c0f4` + working tree.

---

## Status table

| # | Audit issue | Sev. | State | How to verify |
|---|-------------|------|-------|---------------|
| 1 | `not (A or B)` miscompiled on the optimized VM | 🔴 CRITICAL | **Fixed** | `cargo test --no-default-features --test engine_matrix every_construct_behaves_identically_on_tree_and_vm` — cases `not_over_or` and `not_over_or_b_true` (`tests/engine_matrix.rs:120-139`). A failure names the case and engine as `[not_over_or_b_true / vm]`. Guard: `src/vm/opt.rs:313` |
| 2 | Chapter 11 claimed set algebra on `bag` | 🟠 MAJOR | **Fixed** | `doc.html:3246` — "No set algebra — use `set` for that." Enforced by `tools/verify_doc_claims.js` (negative claim: `bag.union` must be unknown) |
| 3 | Cards lacked `fn main()`, so `spid3r run` rejected them | 🟡 | **Fixed** | `doc.html` "Your First Spid3r Program" callout explains the wrap rule; all 16 cards carry `fn main()`. Enforced by `tools/verify_dev_snippets.js` |
| 4 | `yield` / `for x in gen()` doc gap | 🟡 | **Fixed** | `doc.html` keyword table, `yield` row — documents `import "gen"` + `next(it)` and the bare `for-in` caveat |
| 5 | Chapter 02 pipeline table pointed at pre-split paths | 🟡 | **Fixed** | `doc.html:1658` = `src/lexer/mod.rs`, `doc.html:1697` = `src/value/gc.rs` |
| 6 | 8 container constructors missing from chapter 11 | 🟡 | **Fixed** | `linked_list`, `hash_table`, `bst`, `binary_tree`, `priority_queue`, `singly_linked_list`, `doubly_linked_list`, `vec` all present. Enforced by the constructor claims in `tools/verify_doc_claims.js` |
| 7 | Float comments showed `12500` / `0.6` / `0` | 🟡 | **Fixed** | `doc.html:2189` `// 12500.0`; balance comment `// 0.0` |
| 8 | `set` display format and `with_prefix` ordering undocumented | 🟡 | **Fixed** | `docs/STANDARD_LIBRARY.md:817` documents `set{1, 2}`; chapter 11 now states the container display formats and that `bag()` takes no constructor arguments. Enforced by `tools/verify_doc_claims.js` |
| 9 | stdlib export names (`uuid4`, `toml_parse`, `event.new`, bcrypt cost) | 🟡 | **Fixed** | Chapter 14 now carries a "real export names" table (`uuid.uuid_v4`, `toml.toml_parse`, `ev.start`, `bcrypt_hash(pw, cost)` with `cost` 4–31). Enforced by `tools/verify_doc_claims.js`: `uuid4()` must stay an unknown name, and `bcrypt_hash(pw, 3)` must stay rejected |
| 10 | `list.remove` removes by value, undocumented | 🟡 | **Fixed** | `doc.html:3217` — `remove(value)` and `remove_at(i)`. Enforced by `tools/verify_doc_claims.js` |

---

## Post-audit defect found during triage

| # | Issue | State | How to verify |
|---|-------|-------|---------------|
| 11 | `docs/DATA_STRUCTURES.md:262-263` claimed `m.sum(axis)` / `m.mean(axis)` return a 1×N / M×1 **matrix**; they return a flat **vector** | **Fixed** 2026-10-31 | Table rows rewritten; shape paragraph added at `DATA_STRUCTURES.md:274`. Enforced by the six `m.sum`/`m.mean` claims in `tools/verify_doc_claims.js` |
| 12 | 12 orphan `<tr></tr>` rows in `doc.html` (leftovers from hand-deleted table rows) | **Fixed** 2026-10-31 | `node tools/verify_doc_claims.js` → `Empty <tr> rows: 0` |

---

## What keeps this from regressing

| Gate | Covers | Runs in |
|------|--------|---------|
| `tools/verify_dev_snippets.js` | every `.w3-code-body` sample compiles and runs on VM **and** tree; internal anchors resolve; playground URL encoding round-trips | CI `wasm` job |
| `tools/verify_doc_claims.js` | prose claims: constructor coverage, method existence, return types/shapes, display formats, negative claims; table structure (orphan rows, column drift, duplicate ids) | CI `wasm` job |
| `tests/engine_matrix.rs` | engine conformance, including the `not_over_or` optimizer cases | CI `core` job |

The lesson from this audit: **the samples were fine, the prose was wrong.** All
eight doc defects came from tables and sentences that no sample exercised, which
is exactly the gap `verify_doc_claims.js` closes.

### Adding a claim

When you document a fact that a user could act on — a return type, a method
name, a display format, an arity — add it to `CLAIMS` (or `CONTAINERS`) in
`tools/verify_doc_claims.js`, citing the file:line in the `doc` field. Keep the
program's output narrow (a type name, a length, one value) so a display-format
polish elsewhere does not force a rewrite here.

---

## Open items

None at the language or engine level.

The audit's local scratch tree and its HTML report have both been removed as
part of the 2026-10-31 repository cleanup. Nothing in this file depended on
them: every "how to verify" entry above runs against tracked sources, tracked
tests, or the two gates in `tools/`.
