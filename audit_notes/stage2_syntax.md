# Stage 2 — modern syntax by desugaring (interpolation + bitwise)

Date: 2026-10-21 · Toolchain: rustc/cargo 1.99.1 (pinned) · Base: `367b7af`
Scope: the first two syntax features written as *desugarings* rather than as
engine features. Neither backend changed.

The plan listed eight items for this stage. Two landed; the other six are
explained in §5 with the specific machinery each one still needs. I chose depth
over breadth: both features here have full before/after verification, engine
parity in the matrix, and documentation.

## 1. String interpolation

```spid3r
let name = "ada";
let n = 36;
println("hi {name}, {n} years");     // hi ada, 36 years
println("user: {u.name} ({u.age})"); // field paths work
```

`"hi {name}"` becomes `"hi " + string(name)` **in the parser**, so:

- no new AST node, no semantic arm, no engine work;
- the checker reports `unknown name 'ghost'` for a bad hole, because the hole is
  an ordinary `Expr::Ident` by the time it runs;
- every hole is wrapped in `string(...)` (idempotent on strings), so `int`,
  `float` and `bool` interpolate without an explicit conversion;
- `spid3r fmt` leaves the source unchanged (it is token-based and the token
  carries its source range).

### The compatibility trap this avoided

The obvious design — `{{` / `}}` as escapes, as in Python and Rust — **would
have broken the stdlib**. `stdlib/web.matrix` and `examples/web_service.matrix`
implement a runtime template engine that relies on literal `{{ user }}`:

```spid3r
let tpl = "<h1>Welcome, {{ user }}!</h1><p>Role: {{ role }}</p>";
```

`{{` → `{` would silently corrupt every template. So a hole is defined narrowly:

> `{` immediately followed by `identifier(.identifier)*` immediately followed by `}`.

That single rule makes the whole feature additive:

| Input | Result |
|---|---|
| `"hi {name}"` | `hi <value>` |
| `"{{ user }}"` | `{{ user }}` — `{` followed by `{` |
| `"{ x }"` | `{ x }` — `{` followed by a space |
| `"{}"`, `"{0}"` | unchanged |
| `"{not a hole}"` | unchanged |

There is no `{{` escape and therefore no way to write a literal `{name}`; that
is documented rather than papered over.

## 2. Bitwise operators

`& | ^ << >>` with **C precedence**, desugared to calls of five new stdlib
natives (`bit_and`, `bit_or`, `bit_xor`, `shl`, `shr`). One implementation serves
the interpreter, the VM and the AOT backend — the matrix runs all three.

| Expression | Value | Why |
|---|---|---|
| `12 & 10` | `8` | |
| `12 \| 10` | `14` | |
| `12 ^ 10` | `6` | |
| `1 << 4` | `16` | |
| `256 >> 3` | `32` | |
| `-8 >> 1` | `-4` | arithmetic (sign-preserving) shift |
| `1 << 2 + 1` | `8` | shift binds looser than `+` |
| `1 \| 2 ^ 3 & 4` | `3` | `&` > `^` > `\|` |
| `6 & 3 == 2` | error | `==` binds tighter, so this is `6 & (3 == 2)` |

The precedence chain in the parser is hand-written (assign → or → and → cmp →
add → mul → unary), so four levels were inserted: `|`, `^`, `&` between `and` and
`cmp`, and `<<`/`>>` between `cmp` and `add`. `1 << 2 + 1 == 8` is the check that
the shift landed in the right place.

Two deliberate decisions:

- **`^` is xor, not exponentiation.** `doc.html` said "No `**`/`^`. Use
  `pow(b, e)`" — that line is now wrong and was corrected, with an explicit note
  that `^` means xor.
- **A shift amount outside `0..=63` is an error**, not Rust's debug panic /
  release wrap. `1 << 64` reports `shl: shift amount 64 is out of range (0..63)`.
  `1 << 63` still produces `-9223372036854775808`, which is correct two's
  complement.

Non-`int` operands are rejected by the one implementation:
`bit_and expects int operands, found bool`.

## 3. What did not need to change

- **No engine code.** Not the interpreter, the VM compiler, the AOT IR, or the
  bytecode format. Both features reduce to `+`, calls, and existing nodes.
- **No `src/lower` rule.** Interpolation and bitwise are expression-local and
  need no scope information, so the parser desugars them directly. `lower` stays
  the identity, and `lowering_is_the_identity_today` still passes — the seam is
  reserved for rewrites that need to know about scopes or statements.
- **No new AST variant.** That is what keeps the semantic checker and the three
  backends untouched.

## 4. Gates

| Gate | Result |
|---|---|
| `cargo test --no-default-features` | **517 passed, 0 failed** |
| `cargo test --features native --test engine_matrix` | 4/4 — the AOT subset runs both new features |
| `spid3r test` (three-engine differential) | **25 ok, 0 failed** |
| clippy `-D warnings` (core + native + wasm) | 0 |
| `cargo fmt --all -- --check` | clean |
| GUI / AOT suites | 10/10, 6/6 |
| WebAssembly build | OK |

Canonical counts (`tools/count_tests.sh`): **533 tests** (517 core + 10 GUI +
6 AOT).

New coverage: `tests/stage2_syntax.rs` (10) and three matrix rows
(`string_interp`, `bitwise`, `err_bitwise_bool`), the last of which asserts the
same `error[runtime]` text on both engines.

## 5. The other six items, and what each actually needs

Not skipped by preference — each needs machinery this stage did not build:

| Item | Why it is not here |
|---|---|
| `?.` / `??` | Both need a conditional **expression**, and the only sound lowering is a temp variable plus statement hoisting. Hoisting past an earlier side-effecting argument (`g(h(), if c {a} else {b})`) changes evaluation order, so it needs argument spilling (ANF) to stay correct. A native call cannot express `??`'s short-circuit (`x ?? expensive()`), and an IIFE breaks `return` inside a branch. |
| Expression `if` / blocks | Same hoisting problem; it is the prerequisite for `?.`/`??`. |
| Destructuring `let (a, b) = t` | Needs `Stmt::Let` to carry a pattern (AST + semantic + a `lower` rule). A parser-level rewrite cannot emit several statements, and wrapping them in a block would scope the new bindings away. |
| `for (k, v) in map` | Same shape: `Stmt::ForIn` needs a second binding, then `lower` rewrites it to a key loop with a lookup prologue. |
| Default parameters | Needs a `MISSING` sentinel value and a callee prologue; the sentinel must not be observable by user code, which is a design decision worth its own note. |
| `spawn` of any callable | `spawn` currently requires an `fn` literal. Relaxing it means deciding whether the callee is evaluated at spawn time or inside the task, which changes capture semantics. |

The next stage should build the **statement-hoisting pass in `src/lower`** first;
that unlocks `?.`, `??` and expression-`if` together, which is the largest
remaining ergonomic win.
