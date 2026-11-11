# Spid3r bytecode, VM, and runtime

This document describes the on-disk bytecode format (`.matrixc`), the virtual
machine that executes it, the concurrency model, and the memory strategy. All of
it is implemented in `src/vm/` (and the shared value layer in `src/value/`).

## The `.matrixc` file format

A `.matrixc` file is a single serialized top-level **script function** plus
everything it needs to run. The layout, produced by `serialize_vmfunc` and read
back by `deserialize_vmfunc`:

```
SPID3R            — 6-byte magic number (b"SPID3R")
version: u16      — little-endian format version
```

`spid3r compile` writes the **v3 checksummed container**:

```
SPID3R            — 6-byte magic number
version: u16      — 3 (checksummed container)
payload_len: u64  — little-endian byte length of the payload
payload           — a complete plain v2 stream (magic + version 2 + VmFunc)
sha256: [32;]     — SHA-256 of the payload, little-endian bytes in order
```

On load, a v3 container verifies the SHA-256 **before** any structure is
decoded; a mismatch is the clean error `bytecode integrity check failed` (exit
code 4). Legacy plain files (versions 1 and 2) keep the old path — structural
validation only — so previously shipped artifacts stay loadable.

**This is an integrity check, not an authenticity control.** It detects bit
rot, corruption and lazy tampering (QA flipped a single opcode byte and the
pre-checksum reader executed the result silently); it does not prove who wrote
the file, because anyone can recompute the checksum over modified bytes. If
distributed bytecode ever needs authenticity, the control is a signature over
the payload — the in-tree Ed25519 implementation (`src/pkg/ed25519.rs`,
zero dependencies) is the candidate. Not implemented; noted as a future
direction.

The magic + version are checked first; a file missing them (or with an
unsupported version, or truncated) is rejected with a clean `Diagnostic` rather
than parsed as if it were valid bytecode.

### Serialized `VmFunc`

```
name:        opt_str          (1 byte tag + length-prefixed string, or a 0 tag)
arity:       u64              (little-endian)
is_async:    u8
upvalues:    u64 count + count × value   (the captured-cell initial values)
own_upvalues: u8
defaults:    u8 has_defaults; when set: min_arity u64,
             u64 count + count × value   (v2: trailing parameter defaults)
Chunk                          (see below)
```

### Serialized `Chunk`

```
code:          u64 len + len bytes
constants:     u64 count + count × value
functions:     u64 count + count × VmFuncProto
classes:       u64 count + count × VmClassProto
inst_offsets:  u64 count + count × u64   (byte offset where each instruction starts)
inst_spans:    u64 count + count × Span  (source span for each instruction)
pending_span:  Span
modules:       u64 count + count × EmbeddedModule   (see "self-contained imports")
```

Every multi-byte integer is little-endian. Constants may only be `int`, `float`,
or `string` (the compiler never emits other values into the pool), so the value
codec is tiny. All reads go through a bounds-checked `Reader` that returns an
error on truncation instead of indexing out of bounds.

### `VmFuncProto` / `VmClassProto`

A `VmFuncProto` is a nested function prototype: name, arity, async flag,
upvalue descriptor bytes (`u & 0x80 == 0` captures `parent.own_cells[u & 0x7f]`;
`u & 0x80 != 0` captures `parent.func.upvalues[u & 0x7f]`), the count of own
captured locals (`own_upvalues`), and its own `Chunk`. A `VmClassProto` records
the name, optional parent, declared fields (with visibility), and methods
(name, visibility, function-pool index, is-constructor flag).

### `EmbeddedModule` (self-contained imports)

```
key:     string         — canonicalized resolved module path
public:  u64 count + count × string   — exported top-level names
func:    VmFunc         — the module's compiled script function
```

When the compiler sees `import "x"`, it resolves `"x"` to a source file,
parses and semantically checks it, then **recursively compiles and embeds it** —
and all of *its* transitive imports — into the script `Chunk`'s `modules` table.
The table is flattened, so every reachable module is a direct entry keyed by its
canonicalized path. The result is a `.matrixc` that runs under `exec` with **no
`.matrix` source on disk**: when the VM hits `OpImport`, it resolves the module
from this embedded table and runs it in a child VM that shares the globals and
the (flat, transitive) embedded table, so nested imports resolve too.

### The bytecode cache (`cache.bytecode_dir`)

`spid3r run --engine vm` can cache compiled containers: set
`cache.bytecode_dir` in `spid3r.ini` and every run of `foo.matrix` first
looks for `<dir>/<key>.matrixc`, where `key` is the 64-character hex
SHA-256 of **the source bytes plus a compiler epoch** (a fixed byte string
in `src/cli.rs`). Different source, different key, recompiled — and a
compiler / optimizer change for unchanged source also invalidates the
entry when the epoch is bumped. The epoch matters because an old container
still has a valid v3 checksum and would otherwise keep serving pre-upgrade
bytecode (semantically safe, but new optimizations would never take effect,
and an optimized-vs-`--no-opt` comparison would silently measure stale
code).

* **Miss**: the program compiles normally and the v3 container is written
  back (best effort; a failed write just means the next run recompiles).
* **Hit**: the container is loaded through the same `deserialize_vmfunc`
  path as `spid3r exec` — the v3 checksum is verified and `validate_chunk`
  runs before anything executes — and the bytecode runs **instead of**
  compiling. A hit is byte-identical to compiling (same compiler output for
  the same source); the differential contract is covered by
  `tests/phase4_paths.rs` (cached run == fresh run == `exec` of the stored
  container).
* **Corrupt entry**: the checksum fails, the entry is ignored, the program
  compiles fresh, and the write-back replaces the corrupt bytes. A cache can
  never make a run fail or change its output.

`--no-opt` bypasses the cache (it produces different compiler output for the
same source), and the tree engine never touches it (it has no bytecode).
Empty key = disabled. The key is frozen for the agent's remote `config.set`:
a remote peer that could relocate the cache could feed the host pre-built
bytecode.

## The virtual machine

`src/vm/mod.rs` is a register-free **stack machine** in the clox tradition.

- Each `CallFrame` owns a `slots` vector that doubles as both its operand stack
  and its locals. A local's slot is the index where its initializer sits on the
  stack at declaration time, so `add_local` records the current slot count.
- A function call pushes a new frame and `continue`s the main loop; `OpReturn`
  pops the frame and pushes the return value onto the caller's stack.
- Functions and closures are `Arc<VmFunc>` carrying **upvalue cells** — shared
  `Arc<Mutex<Value>>` slots. Current-frame captured locals are routed through
  `frame.captured` (`GetLocal`/`SetLocal`), while enclosing captures live in
  `func.upvalues` (`GetUpvalue`/`SetUpvalue`). This makes closures correct across
  arbitrary nesting, loops, and after the defining frame has returned, and keeps
  them `Send` so they can be captured by a `spawn`ed task.
- All value operations (construct, method dispatch, indexing, length, iteration,
  equality, display) are delegated to the shared `value::collections` layer, so
  the VM stays semantically identical to the tree-walk interpreter.
- Exception handling (`try`/`catch`/`finally`/`throw`) is driven by a handler
  stack the VM owns; unwinding restores the operand-stack height and releases any
  mutex guards held by the unwound frames.
- `super` compiles to `SuperCtor` (opcode 56: parent class name index `u16`,
  argc `u8`) and `SuperMethod` (opcode 57: parent class name index `u16`,
  method name index `u16`, argc `u8`). The receiver is the executing frame's
  `self` (slot 0); the parent class is resolved by name in the globals, so the
  opcodes carry no class-table state. The opcodes are additive (byte 55 is
  `DefineConst`, which records a top-level `const` so the VM can reject a
  rebind and report a module's `public const` exports); `.matrixc` files
  written by older builds still load.
- `?.` compiles to `FieldGetSoft` (opcode 58: name index `u16`, F-4
  2026-10-25) — like `FieldGet` except a null receiver and a missing map key
  push `null`. Additive per docs/BYTECODE_STABILITY.md: no format-version
  bump, older readers reject the byte cleanly.
- Assignment lowering compiles to `Rotate` (opcode 59: depth `u8`,
  2026-11) — moves the value `depth` positions down to the top of the stack
  (`a b c d` with depth 4 → `b c d a`). The tree interpreter (the oracle)
  evaluates an assignment's RHS before the target's object/index expressions,
  so the compiler emits the RHS first and uses `Rotate` to restore
  `IndexSet`/`FieldSet`'s `[target, value]` order afterwards. Additive per
  docs/BYTECODE_STABILITY.md: no format-version bump, older readers reject
  the byte cleanly.

- **Runtime fused superinstructions** (Phase 1): the dispatch loop peeks at a
  few short opcode patterns — `GetLocal;GetLocal;Mul;GetLocal;LessEq;JumpIfFalse`,
  `GetLocal;GetLocal;Mod;Constant;Equal;JumpIfFalse`, and the
  `"prefix" + string(n % k)` key-building shape — and executes them in one step.
  These are **not new opcodes**: there is no `OpCode` variant, no `Chunk` field,
  no optimizer edit and no `.matrixc` format change, so v1/v2/v3 containers are
  untouched. As of Phase 5 the fusions run only on the default optimized VM
  path: `run --engine vm --no-opt` calls `Vm::set_opt_enabled(false)`, which
  gates the runtime peeks in addition to the compile-time passes. Per-VM inline
  caches stay on under `--no-opt` (memoization, not instruction peeks), and no
  `OpCode`, `Chunk` field or `.matrixc` format change is involved.
  A fusion is taken only when `--prof`, `--coverage`, the debugger,
  `pending_str` and captured locals are all absent, so instrumentation still
  observes every original opcode and line hit. That gate covers the
  `GetLocal a; GetLocal b; Less; JumpIfFalse` and
  `GetLocal a; GetLocal b; Add; SetLocal; Pop` pair fusions as well: they
  used to run under `--prof`, which made the profiler count a fused pair as
  ONE instruction and under-count the original opcode sequence by ~23%.
  Phase 5 closes that gap, so `--prof` reports every original opcode in
  both the default and the `--no-opt` path and their difference is exactly
  the compile-time rotation effect. Integer fast paths use
  `checked_mul` / `checked_rem` and fall through to the generic handlers on
  overflow or modulo by zero; a global such as `string` is resolved from the
  live globals map on every hit, so a user-defined binding is never fused.
  Phase 2 also accepts `JumpIfTrue` after the `LessEq` pattern, because the
  optimizer can rotate that loop condition to the bottom of the loop; both
  branch polarities take the same fused path.

- **Compile-time loop back-edge rotation** (Phase 2): `optimize_script()`
  rewrites a canonical `while` loop from

  ```text
  S: <cond>; JumpIfFalse end; <body>; Jump S; end:
  ```

  into

  ```text
  S: <cond>; JumpIfFalse end; <body>; <cond (duplicated)>; JumpIfTrue body_start; end:
  ```

  The top check still runs on entry and on `continue` (which targets `S`),
  and `break` still targets `end`. Steady-state iterations jump straight back
  to the body, removing exactly **one unconditional `Jump` dispatch per
  iteration**. The condition block is duplicated, never reordered, so every
  evaluation still happens once, in the same order, with the same stack
  effect. `inst_offsets` / `inst_spans` are rebuilt so debugger and coverage
  line attribution still point at real source lines.

  This is a **real optimizer-only transform**: it lives entirely in
  `src/vm/opt.rs`, so `compile_program_unoptimized` (`--no-opt`) never sees
  it. No new `OpCode`, no `Chunk` field, no `.matrixc` format change — v1/v2/v3
  containers and the bytecode checksum are untouched. Guardrails: chunks with
  `Try` are skipped; only a whitelist of pure condition opcodes is duplicated
  (no internal jumps, calls, stores or stack shuffling); at least one body
  instruction is required; jumps from outside landing inside the loop, or on
  the back-edge itself, veto the transform; the new offset must fit `i16`.
  `OptStats.rotations` is the per-pass counter.

  Because the pass removes work at **compile** time, `--prof` on the default
  path legitimately reports fewer executed instructions than
  `--prof --no-opt` — one `Jump` per loop iteration. Runtime fusions are
  disabled both under instrumentation and under `--no-opt`, so no opcode is
  hidden by a fast path; `tests/engine_matrix.rs` pins the exact delta and
  `tests/phase5_perf_parity.rs` pins `--no-opt` output parity.

### Bytecode validation (safety net)

The validator runs at load time, **before any instruction executes**, and
recurses into nested function prototypes, class methods, and embedded module
functions (each with its own prototype's upvalue count). It rejects:

- invalid opcodes, truncated instructions, non-increasing `inst_offsets`,
  offsets past the code, jump targets that are not instruction boundaries,
  a chunk that can fall off its end, and trailing bytes;
- pool indices out of range for `Constant`, `GetGlobal`, `SetGlobal`,
  `DefineGlobal`, `DefineConst`, `FieldGet`, `FieldSet`, `Import`,
  `ImportAs`, `Construct`, `Invoke`, `Closure`, `Class`, and the `Super*`
  opcodes;
- **wrong value kinds**: every name operand (all of the above except
  `Constant`) must be a `string` constant; the `Constant` pool itself may
  only hold `int`/`float`/`string` (bool/null use dedicated opcodes);
- `GetUpvalue`/`SetUpvalue` indices outside the prototype's declared upvalue
  list — otherwise a crafted artifact would silently read `null` for a cell
  that does not exist.

The VM is defensive at every use site anyway (`match` on the value tag,
`.get()` with a fallback), so a validator gap degrades to a clean runtime
error, never to undefined behavior. See `docs/AUDIT_REPORT.md`.

Before any chunk is executed, `validate_chunk` walks it (and all nested function
prototypes and embedded module chunks) recursively and rejects:

- invalid opcodes,
- truncated instructions (operand bytes running past the end of `code`),
- constant/function/class pool references out of range,
- negative or out-of-bounds jump targets and targets that do not land on an
  instruction boundary (checked with a binary search over the instruction
  starts, so validation stays `O(n log n)` — a 10 MB file with many jumps used to
  take ~10¹³ comparisons),
- an empty code section, or a chunk whose last instruction can fall through
  (`return`, `throw`, or `jump` is required) — the VM advances `ip` past the end
  of a fall-through chunk,
- instruction metadata that does not line up with the code:
  `inst_offsets.len() != inst_spans.len()`, non-increasing offsets, or an offset
  past the code section. `span_at` indexes through `inst_spans` with `get`, so a
  crafted file cannot panic the host even if validation were skipped.

Additionally, `deserialize_vmfunc` validates every serialized item count against
the remaining byte slice before allocating vectors, caps function/module nesting
at `MAX_DESERIALIZE_DEPTH` (96 — the parser's own limit is 64), and rejects
**trailing bytes** after the last structure. The opcode decoder itself is a safe
`OpCode::from_u8` table (no `mem::transmute`), the VM's instruction fetch is
bounds-checked (`ip` outside the code section is a clean VM error, not an index
panic), and every pool read / jump target is bounds-checked at run time as well.

## Concurrency model

Concurrency is provided by a threaded scheduler (`src/sched/`) shared between the
interpreter and the VM:

- `spawn <callable>` runs the callable on a worker thread. The worker VM shares
  the parent's globals, scheduler, module cache, **and** embedded-module table, so
  a spawned task sees the same world.
- `await <task>` joins the task and pushes its result; `await <channel>` receives
  from a channel.
- `channel` produces a typed queue; `mutex` guards a shared cell. Mutex guards
  acquired inside a frame are released on return or unwinding (like `finally`).

Because upvalue cells are `Arc` and the VM shares state through `Arc<Mutex<…>>`,
closures and globals remain correct across threads.

## Memory strategy

Dynamic values use thread-safe reference counting (`Arc<Mutex<…>>`) across both
the tree-walk interpreter and the bytecode VM so values and closures can safely
cross `spawn` task boundaries:

- **Cycle collector (`src/value/gc.rs`):** Every container and instance type
  capable of forming reference cycles (`list`, `vec`, `map`, `set`, linked
  lists, trees, `graph`, `heap`, `matrix`, and class instances — 20+ types)
  are registered with a mark-and-sweep cycle collector (`gc_collect()`,
  `gc_live_cells()`, and automatic threshold collection).
- **Weak references (`weak`):** Non-owning weak references (`weak(x)`, `.get()`,
  `.alive()`) allow caches and parent pointers without extending target lifetimes.
- **Cycle-safe operations:** `display_value`, `values_equal`, `deep_clone`, and
  `json_write` track visited container pointers and drop lock guards before
  recursing, preventing deadlocks and stack overflows on self-referential graphs.

