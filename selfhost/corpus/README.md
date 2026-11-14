# The frozen self-host corpus (SH-4)

This directory is the **exact program set** every self-host parity gate
(Phases 1–7 of `full_plan.html`) runs against. `MANIFEST.sha256` pins every
file; `python tools/corpus_manifest.py verify` re-checks it (CI runs this —
drift blocks the merge). **Changing a corpus program requires a ledger entry
in `full_plan.html` stating why.**

| Directory | Count | Contents |
|---|---|---|
| `examples/` | 24 | copies of the runnable `examples/*.matrix` (generated, header-stamped) |
| `engine_matrix/` | 87 | programs extracted from `tests/engine_matrix.rs` (generated, header-stamped) |
| `edge/` | 9 | hand-built: closures, try/finally, integer edges, unicode, OOP, ordered concurrency, interpolation, sibling-module imports (`namespaces.matrix` + `edge_lib.matrix` are a pair) |
| `negative/` | 13 | one program per error diagnostic class: `error[lexer]` ×2, `error[parser]` ×3, `error[semantic]` ×5, `error[runtime]` ×2, `error[usage]`/`error[build]` ×1 (`ffi_missing_symbol` — build-time on AOT, usage on the script engines) |

Regenerate the generated parts after changing `examples/` or
`tests/engine_matrix.rs` with `python tools/corpus_manifest.py build`, then
record the change in the ledger. The `edge/` and `negative/` programs are
authored in place.

Multi-file note: `edge/namespaces.matrix` imports `edge_lib.matrix` from the
same directory (sibling resolution, like `examples/modules.matrix`). Parity
harnesses must run programs from their own directory.
