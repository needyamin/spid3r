# Stage 3 — enums (sum types) and `match`

Date: 2026-10-21 · Toolchain: rustc/cargo 1.99.1 (pinned) · Base: `b9d7c17`
Scope: the roadmap's largest language feature. **No backend changed** — both
`enum` and `match` lower to constructs the engines already execute.

```spidr
enum Shape {
    Empty,
    Circle(radius),
    Rect(w, h),
}

fn area(s) {
    return match (s) {
        Circle(r) => 3 * r * r,
        Rect(w, h) => w * h,
        Empty() => 0,
    };
}
```

## 1. Representation: why a variant is a map and a constructor is a function

A sum type needs a tag plus a payload. Three options were on the table:

| Option | Verdict |
|---|---|
| A new `Value::Variant` | Touches `value/`, display, equality, hashing, GC and all three engines. Rejected: engine work for something the language can express. |
| A synthesized class per enum | Needs class construction at lowering time and still gives no cross-module story. |
| **A map with a `__tag` key** | Maps already work everywhere — display, equality, GC, AOT — and `m.field` is key lookup in all three engines (fixed in Stage 2b). |

The declaration lowers to **one public constructor function per variant**:

```spidr
// enum Shape { Circle(radius) }  becomes
public fn Circle(radius) {
    let __m = map();
    __m["__tag"] = "Circle";
    __m["radius"] = radius;
    return __m;
}
```

That single choice buys three things:

1. **Construction is an ordinary call.** `Circle(1)` needs no new AST node, no
   lowering at the use site, and no engine support.
2. **Modules work for free.** Because the constructor is a `public fn`, an
   importing file calls it like any other function — there is no
   name-resolution step that would have to know which enum a bare `Circle`
   belongs to.
3. **`match` only reads a map key.** No new opcode, no type test instruction.

`public enum` therefore exports its **variants**, not the enum name
(`modules::public_names_of_in`). That was the one place that needed a change: the
first version of the import test failed with `unknown name 'Circle'` because the
export collector only knew about `fn`/`class`/`interface`/`let`/`const`.

## 2. `match` lowering

A match becomes a **linear** chain, not a nested tree:

```text
let __s = <scrutinee>;        // evaluated exactly once
let __t = null;               // the result
let __d = false;              // "an arm already ran"
if (not __d and __s["__tag"] == "Circle") { let r = __s["radius"]; <body>; __d = true }
if (not __d and __s["__tag"] == "Rect")   { let w = …; let h = …; <body>; __d = true }
…
if (not __d) { throw "no match arm matched the value (the match is not exhaustive)" }
```

The `__d` flag is the important part. The obvious compilation — an `if/else if`
chain — has to duplicate the remaining arms into the `else` of every *guard*,
which is exponential in the number of guarded arms. With the flag, a failed guard
just leaves `__d` false and the next arm's `if` runs. Each arm is one statement.

Patterns compile to a condition plus bindings:

| Pattern | Condition | Bindings |
|---|---|---|
| `_` | `true` | — |
| `x` | `true` | `let x = __s` |
| `0`, `"a"` | `__s == 0` | — |
| `Circle(r)` | `__s["__tag"] == "Circle"` | `let r = __s["radius"]` |
| `Dot(At(x, y))` | tag tests, `and`-ed | `let x = __s["p"]["x"]` … |

Sub-patterns recurse on the field expression, so nesting needs no extra code.

## 3. Exhaustiveness, in a dynamically typed language

The scrutinee's static type is usually `Any`, so the checker cannot simply look
up "the enum of this value". The rule used instead:

> If every variant pattern in a match belongs to **one** enum and there is no
> `_` or binding arm, that enum must be fully covered.

That catches the common mistake:

```
error[semantic]: this match is not exhaustive for enum 'Color': missing 'Blue'
                 (add an arm or `_`)
```

Anything ambiguous — literals only, variants from two enums, a catch-all — is
left to the runtime, where an unmatched value throws a normal error rather than
silently producing `null`:

```
error[runtime]: uncaught throw (thrown: no match arm matched the value …)
```

## 4. Syntax decisions

- `=> expr` and `=> { … }` are both accepted; a bare expression is wrapped in a
  one-statement block.
- A `match` in **statement position** needs no trailing `;` (it is dispatched
  from `parse_statement`, like `if`), but `match (v) { … };` also parses.
- Pattern bindings are scoped to their arm (`push_scope`/`pop_scope` around the
  guard and body).
- A bare identifier pattern is always a **binding**, never a nullary variant —
  nullary variants must be written `Empty()`. That removes the classic
  uppercase/lowercase guessing game.
- `enum` and `match` are now keywords. I checked `examples/` and `stdlib/` for
  existing uses as identifiers: there were none.

## 5. Gates

| Gate | Result |
|---|---|
| `cargo test --no-default-features` | **543 passed, 0 failed** |
| `cargo test --features native --test engine_matrix` | 5/5 — the AOT subset runs `enum_match`, `match_guards`, `match_literals` |
| `spid3r test` (three-engine differential) | **25 ok, 0 failed** |
| clippy `-D warnings` (core + native) | 0 |
| `cargo fmt --all -- --check` | clean |
| GUI / AOT suites | 10/10, 6/6 |
| WebAssembly build | OK |

Canonical counts: **559 tests** (543 core + 10 GUI + 6 AOT).

New coverage: `tests/stage3_match.rs` (11) and four matrix rows, including
`err_match_not_exhaustive`, which asserts the exact diagnostic text.

## 6. Limits, stated plainly

| Limit | Consequence |
|---|---|
| Variant names are top-level function names | Two enums cannot share a variant name, and a variant cannot share a name with a function. The checker reports `'X' is declared twice`. |
| A variant is a mutable map | `c.field = v` would mutate the variant. Nothing prevents it; a future version could freeze enum payloads. |
| `type_of(variant)` is `map` | There is no nominal enum type in the type system, so `let s: Shape = …` is `Any`. This is why exhaustiveness is inferred from the arms rather than from a type. |
| No `if let` / `while let` | Deferred; the match chain covers the same ground today. |
| No or-patterns (`A() \| B()`) | Deferred. |
| Exhaustiveness across modules | The check uses the *local* enum table, so a match in an importing file is not checked for exhaustiveness (it runs and throws on an unmatched value instead). |

### Cross-module matching

`lower` only sees the file it is lowering, so a `match` in an importing file
cannot know the field names of a foreign variant. The first version simply
skipped those bindings, and the arm body then failed with
`error[runtime]: unknown name 'x'` — which the test caught.

The fix is to make the value carry its own shape: each constructor also stores
`__m["__fields"] = ["radius"]`, and a pattern whose variant is unknown binds by
position through that list:

```spidr
let __fl = __s["__fields"];
let x = __s[__fl[0]];
```

So a match on an imported enum works, at the cost of one extra list per variant
value. `tests/stage3_match.rs::a_match_on_an_imported_enum_binds_its_payload`
covers it on both engines.
