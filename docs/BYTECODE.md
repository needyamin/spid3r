# Spid3r bytecode, VM, and runtime

This document describes the on-disk bytecode format (`.matrixc`), the virtual
machine that executes it, the concurrency model, and the memory strategy. All of
it is implemented in `src/vm/` (and the shared value layer in `src/value/`).

## The `.matrixc` file format

A `.matrixc` file is a single serialized top-level **script function** plus
everything it needs to run. The layout, produced by `serialize_vmfunc` and read
back by `deserialize_vmfunc`:

```
SPID3R        — 6-byte magic number (b"SPID3R")
version: u16  — little-endian format version (currently 1)
VmFunc        — the script function (see below)
```

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

A `VmFuncProto` is a nested function: name, arity, async flag, the enclosing
frame's upvalue-slot indices, the count of own captured locals, and its own
`Chunk`. A `VmClassProto` records the name, optional parent, declared fields
(with visibility), and methods (name, visibility, function-pool index,
is-constructor flag).

### `EmbeddedModule` (self-contained imports)

```
key:     string         — the import literal, e.g. "math_util"
public:  u64 count + count × string   — exported top-level names
func:    VmFunc         — the module's compiled script function
```

When the compiler sees `import "x"`, it resolves `"x"` to a source file,
parses and semantically checks it, then **recursively compiles and embeds it** —
and all of *its* transitive imports — into the script `Chunk`'s `modules` table.
The table is flattened, so every reachable module is a direct entry keyed by its
import literal. The result is a `.matrixc` that runs under `exec` with **no
`.matrix` source on disk**: when the VM hits `OpImport`, it resolves the module
from this embedded table and runs it in a child VM that shares the globals and
the (flat, transitive) embedded table, so nested imports resolve too.

## The virtual machine

`src/vm/mod.rs` is a register-free **stack machine** in the clox tradition.

- Each `CallFrame` owns a `slots` vector that doubles as both its operand stack
  and its locals. A local's slot is the index where its initializer sits on the
  stack at declaration time, so `add_local` records the current slot count.
- A function call pushes a new frame and `continue`s the main loop; `OpReturn`
  pops the frame and pushes the return value onto the caller's stack.
- Functions and closures are `Arc<VmFunc>` carrying **upvalue cells** — shared
  `Arc<...>` slots holding `Value`. This makes closures correct across arbitrary
  nesting and after the defining frame has returned, and keeps them `Send` so
  they can be captured by a `spawn`ed task.
- All value operations (construct, method dispatch, indexing, length, iteration,
  equality, display) are delegated to the shared `value::collections` layer, so
  the VM stays semantically identical to the tree-walk interpreter.
- Exception handling (`try`/`catch`/`finally`/`throw`) is driven by a handler
  stack the VM owns; unwinding restores the operand-stack height and releases any
  mutex guards held by the unwound frames.

### Bytecode validation (safety net)

Before any chunk is executed, `validate_chunk` walks it recursively and rejects:

- invalid opcodes,
- truncated instructions (operand bytes running past the end of `code`),
- constant/function/class pool references out of range,
- jump targets that do not land on an instruction boundary.

This is the spec-required guard against malformed or hand-edited `.matrixc`
files; it reports a clean `Diagnostic` rather than panicking or invoking
undefined behaviour. The opcode decoder itself is a safe `OpCode::from_u8`
table (no `mem::transmute`), and every pool read / jump target is bounds-checked
at run time as well.

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

## Memory strategy and limitations

Dynamic values use reference counting:

- The tree-walk interpreter uses `Rc<RefCell<…>>` for shared mutable state
  (e.g. collection nodes, closures).
- The VM uses `Arc` (and `Arc<Mutex<…>>` for cross-thread values) so values and
  closures can cross `spawn` boundaries.

**Known limitation — reference cycles leak.** There is no cycle collector. A
data structure with a reference cycle (for example, a doubly linked list whose
nodes point at one another, or an object that references its parent which
references it back) will not be reclaimed until the whole program exits. This is
a documented, accepted trade-off for the zero-dependency `Rc`/`Arc` design; avoid
long-lived cycles in programs that must run for a long time.

Other current limitations are listed in [`ROADMAP.md`](ROADMAP.md) (no bytecode
optimizer, no `fmt`/debugger, no native/AOT backend, no package manager, and
inert type annotations).
