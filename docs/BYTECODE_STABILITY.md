# Bytecode (`.matrixc`) stability & compatibility policy

Status: **Adopted** (2026-10-22, DOC-01). This is the compatibility promise the
project makes about compiled bytecode artifacts. It formalizes what
`docs/BYTECODE.md` describes mechanically. Issue: ARC/DOC-01 in
`architect_report.txt` §5.

## The promise

**Backward compatibility within a major version line.** A `.matrixc` artifact
written by any Spid3r `1.x` build **must load and run on every later `1.x`
build** of the same platform. Newer readers accept everything older writers
produced. This is the promise deployers rely on: compile once, ship the
artifact, update the runtime without recompiling.

**Forward compatibility is clean rejection, not correctness.** An *older* build
reading an *newer* artifact must fail with a clean, non-panicking `Diagnostic`
(never undefined behavior). It is allowed to fail; it is not allowed to
misinterpret. This already holds: the magic check, the version check, the
bounds-checked deserializer, the `OpCode::from_u8` table, and `validate_chunk`
all reject before any instruction executes.

**No cross-major-version promise.** `2.0` may break the format. (There is no
`2.0` today; this paragraph reserves the semantic.)

## What "a change to the format" means

The serialized layout is defined in `docs/BYTECODE.md` (magic `SPID3R`,
`version: u16`, then the `VmFunc` tree: code, constant pool, function/class
prototypes, span tables, embedded modules). Changes fall into three classes:

### 1. Additive changes — NO version bump (keep version = 1)

Anything that leaves existing bytes meaningful under the old reading rules:

- **New opcodes** on previously-free bytes (the `SuperCtor`/`SuperMethod`/
  `DefineConst` additions were exactly this). An older reader rejects the new
  opcode cleanly — acceptable forward behavior — while a newer reader accepts
  all old files.
- **New trailing structures** appended *after* a version-gated point, or in a
  position old readers never reach.
- Widening validation (rejecting more crafted inputs) — readers get stricter,
  writers' output is unchanged.

Rule: if a `1.(n-1)` artifact is byte-identical in meaning to a `1.n` reader,
the version stays.

### 2. Interpreted-differently or restructured changes — bump `version`

Any change where the same bytes would decode to a *different* program:

- reordering, resizing, or repurposing existing fields;
- changing the constant-pool value kinds (int/float/string today);
- changing upvalue-descriptor encoding;
- removing or renumbering an opcode.

On a bump: the reader keeps accepting **all previous major-line versions it
knows** (a version → decoder table, not a single `==` check), and rejects
versions it does not know with a clean diagnostic naming the found and newest
supported version.

### 3. Excluded from the promise

- **In-memory `Chunk`/`Value` layout** — not an artifact; may change freely.
- **`.matrixc` produced by a *newer* minor than the reader** — see "forward
  compatibility" above: clean rejection is the contract.
- **Behavior of the optimizer** — an artifact's *result* is pinned by the
  engine contract (tree == vm == native on observables), not by its byte
  sequence. A future release may compile the same source to different bytes.
- **Source-level (`.matrix`) compatibility** — governed by the language
  version, not this document.

## Reader obligations (checklist when touching `vm/bytecode.rs` / `vm/serializer.rs`)

1. Old-file acceptance: the existing `.matrixc` fixtures in
   `tests/bytecode_*.rs` (round-trip + crafted-rejection suites) must stay
   green. If you changed the writer, verify old artifacts still load with a
   pre-change binary before merging.
2. New constructs must go through `validate_chunk` (pool indices, instruction
   boundaries, value kinds) **before** the first release that emits them.
3. Unknown version → `Diagnostic`, message includes both the file's version
   and the reader's max supported version. Never `panic!`, never
   `unreachable!()`.
4. Deserializer item counts are checked against remaining bytes before
   allocation; nesting stays capped at `MAX_DESERIALIZE_DEPTH` (128).
5. Update `docs/BYTECODE.md` (format) and this file (promise class of the
   change) in the same PR.

## Writer obligations

1. Write `version = 1` until class-2 change forces a bump; when bumping, the
   writer writes the *minimum* version that can represent the artifact, so a
   program using no new constructs still produces an old-line file when the
   decoder table allows it.
2. Determinism: identical source + options ⇒ byte-identical artifact (span
   tables included). Reproducible builds depend on this; a test asserts it.

## Status today

- Current format version: **2** (2026-10-22 — v2 added trailing constant
  parameter defaults to function prototypes; the reader accepts v1 and v2,
  the writer emits v2). Magic: `SPID3R` (6 bytes).
- All changes so far (opcodes 55–57, embedded modules, validator hardening)
  were class-1 additive; the promise above holds retroactively for every
  shipped artifact.
