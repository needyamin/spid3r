# Stage 6 — actionable diagnostics

Date: 2026-10-21 · Toolchain: rustc/cargo 1.99.1 (pinned) · Base: `18750f7`
Scope: the audit's complaint that errors are "not actionable" — no source
snippet, no caret, no suggestion, no machine-readable form.

Before:

```
error[semantic]: type mismatch in 'xs': expected list<int>, got list<string>
  --> app.matrix:2:25
```

After:

```
error[semantic]: type mismatch in 'xs': expected list<int>, got list<string>
  --> app.matrix:2:25
  |
2 |     let xs: list<int> = ["a"];
  |                         ^^^^^
  = help: annotations are checked before the program runs; use `any` to opt out,
          or convert explicitly with string(x) / int(x) / float(x)
```

## 1. Snippet rendering

`Diagnostic::render()` composes message → location → snippet → `= note:`/`= help:`
→ stack trace. Three details matter:

| Detail | Why |
|---|---|
| The caret is placed with **character** columns | Stage 0 made `Span.col`/`len` character-based precisely so this would line up; the test uses a line with `ααα` before the error. |
| Tabs are expanded to four spaces | A tab in the source would otherwise put the caret in the wrong place. |
| The source is read from `span.file`, and a failure falls back to the plain form | Diagnostics can come from in-memory sources, deleted files, or synthetic `<memory>` spans. Guessing a snippet would be worse than omitting it. |

`Display` is unchanged, so every existing consumer (tests, the WASM bridge, the
embedding API) keeps the compact form; `render_with(Some(source))` is there for
callers that already hold the text.

## 2. `= help:`

Help is attached where it changes what the user does. The first (and so far only)
site is the type-mismatch family — six call sites now go through
`Checker::mismatch`, which appends the suggestion to either use `any` or convert
explicitly. That is the diagnostic users hit most often while learning the
gradual type system.

Notes and help live in one boxed field rather than two inline ones — see §4.

## 3. `check --json`

```json
{"phase":"semantic","message":"type mismatch in 'xs': expected list<int>, got list<string>",
 "file":"app.matrix","line":2,"col":25,"len":5,
 "help":"annotations are checked before the program runs; …","notes":[],"trace":[]}
```

One object, one line, printed on stdout (the human form goes to stderr), so a CI
annotation step or an editor can consume it without scraping. `--json` is
consumed by the CLI dispatcher rather than by `check` itself, because it is a
property of the *error printer*, not of the subcommand; it is only recognized for
`check` and `compile`, so it can never be mistaken for a script argument.

## 4. The bug this stage caused, and the lesson

Adding `help: Option<String>` and `notes: Vec<String>` inline grew `Diagnostic`
by 48 bytes — and `tests/artifact_hardening::deeply_nested_bytecode_hits_the_depth_limit`
immediately died with `STATUS_STACK_OVERFLOW`.

`Diagnostic` sits in **every** `Result`, including the recursive bytecode
serializer/deserializer, where its size is multiplied by the nesting depth. That
test builds 200 levels; 48 bytes × 200 was enough to tip a 2 MB test-thread
stack. I verified the causality by stashing the change and re-running the test on
the previous tree (it passed there).

The fix is one boxed field:

```rust
pub extras: Option<Box<DiagnosticExtras>>,   // 8 bytes, was 48
```

That is a real constraint on this type, not a micro-optimisation: **anything
added to `Diagnostic` is multiplied by the recursion depth of the code that
propagates it.** It is now recorded in the struct's doc comment.

The serializer itself is still unbounded (the reader caps at 128, and the parser
caps nesting at 64, so only a hand-built `VmFunc` can reach it); that is
documented on `serialize_vmfunc` rather than fixed, because the realistic
exposure is a Rust caller constructing a value no program can produce.

## 5. Gates

| Gate | Result |
|---|---|
| `cargo test --no-default-features` | **568 passed, 0 failed** |
| `spid3r test` (three-engine differential) | **25 ok, 0 failed** |
| clippy `-D warnings` (core + native) | 0 |
| `cargo fmt --all -- --check` | clean |

Canonical counts: **584 tests** (568 core + 10 GUI + 6 AOT).

New coverage: `tests/stage6_diagnostics.rs` (6) — snippet presence, caret
alignment on ASCII and non-ASCII lines, the help line, the JSON shape, a runtime
error's snippet, and the plain fallback for a synthetic span.

## 6. What is still missing from this area

| Item | Why it is not here |
|---|---|
| Stable error **codes** (`E0308`) | A code is only useful if it is stable per *class* of error, which means classifying ~200 construction sites. Doing it halfway (codes on some errors, phase names on others) is worse than the current uniform `error[phase]`. |
| Multi-span labels ("defined here", "first borrowed here") | Needs a second span on `Diagnostic` and per-site work; the single-span snippet covers the common case. |
| Warnings | `check_file` returns `Result<()>`; a warning layer needs it to return a list, plus a `--deny`/`--allow` policy. That is a stage of its own (the audit's "no warnings at all"). |
| `--json` for `run` | Runtime diagnostics are printed from the same funnel, but a JSON *stream* for a running program needs a decision on how stdout/stderr interleave. |

Warnings are the highest-value follow-up: the checker currently has no way to say
"this is suspicious" without failing the build.
