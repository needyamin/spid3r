# Stage 4 — the error model: `option`/`result` typing, `?`, combinators

Date: 2026-10-21 · Toolchain: rustc/cargo 1.99.1 (pinned) · Base: `41db193`
Scope: make the two built-in sum types actually usable. `?` and the combinators
are new; the typing fix repairs a defect that made the annotations dead on
arrival.

## 1. The defect: `option<T>` could not be annotated at all

```spidr
let x: option<int> = some(5);
// error[semantic]: type mismatch in 'x': expected option, got some
```

`resolve_type("option")` produced `Ty::Coll("option")`, while
`type_of(some(5))` produced `Ty::Coll("some")` — and assignability compared the
collection *names*. So the annotation the language documents was impossible to
use, and `option`/`result` were runtime-only values.

The fix normalises the constructor name where the type is computed:

```rust
fn coll_kind(name: &str) -> &str {
    match name {
        "some" | "none" => "option",
        "ok" | "err" => "result",
        other => other,
    }
}
```

## 2. The second half: element types were erased

`(Ty::Coll(a, _), Ty::Coll(b, _)) => a == b` ignored the element, so
`let xs: list<int> = ["a"]` passed. Annotations bought nothing inside a
container. Now the elements are compared, with two deliberate exemptions:

| Case | Why |
|---|---|
| Either side has no element type (`[]`, `none()`, `map()`) | There is nothing to compare; treating it as incompatible would reject the obvious empty-literal case. |
| `result` | `result<int>` cannot name the error side, so comparing would reject `let r: result<int> = err("bad")`. |

`Ty::name` also prints the element now, because the old message was
`expected list, got list`:

```
error[semantic]: type mismatch in 'xs': expected list<int>, got list<string>
```

## 3. `?` propagation

```spidr
fn half(n) {
    if (n % 2 != 0) { return err("odd"); }
    return ok(n / 2);
}

fn quarter(n) {
    let h = half(n)?;      // returns err("odd") to the caller
    return ok(h / 2);
}
```

Lowered, not interpreted:

```spidr
let __v = <expr>;
if (is_failure(__v)) { return __v; }
__v.unwrap()
```

Returning the **container** (not the bare error) is what makes it compose: an
`option` propagates `none`, a `result` propagates `err(e)`, and the caller sees
the same shape it would have returned itself.

`is_failure` is a normal builtin rather than a hidden helper, so the same
predicate is available in user code. `?` on a non-option is a clean runtime
error (`is_failure expects an option or a result, found int`) rather than a
panic.

## 4. Combinators

| `option` | `result` |
|---|---|
| `map(f)` | `map(f)` |
| `and_then(f)` | `and_then(f)` |
| `ok_or(e)` | `map_err(f)` |
| `unwrap_or_else(f)` | `unwrap_or_else(f)` |
| `flatten` | `flatten` |

`result.unwrap_or_else(f)` receives the error, matching Rust's
`FnOnce(E) -> T`; `option.unwrap_or_else(f)` takes no argument.

**This needed one engine change.** The combinators take callbacks, and the VM
only routed *list* methods through a callback host — `option.map(f)` fell through
to the no-callbacks dispatcher and failed with

```
error[runtime]: higher-order methods with callbacks are not supported in this context
```

while the tree engine ran the same program fine. The VM now has an
`Option`/`Result` arm in its method dispatch that reuses the same `VmHost`
adapter `list.map` uses, and `and_then`/`map_err`/`unwrap_or_else` were added to
`is_callback_method` so the dispatch notices them. AOT already worked (it routes
through the runtime's own host).

## 5. Gates

| Gate | Result |
|---|---|
| `cargo test --no-default-features` | **554 passed, 0 failed** |
| `cargo test --features native --test engine_matrix` | 5/5 — the AOT subset runs the annotation, `?` and combinator cases |
| `spid3r test` (three-engine differential) | **25 ok, 0 failed** |
| clippy `-D warnings` (core + native) | 0 |
| `cargo fmt --all -- --check` | clean |

Canonical counts: **570 tests** (554 core + 10 GUI + 6 AOT).

New coverage: `tests/stage4_errors.rs` (11) and four matrix rows
(`option_result_annotations`, `propagate`, `error_combinators`,
`err_element_type`).

## 6. What this stage did not do

| Planned | Status |
|---|---|
| Typed / multiple `catch` clauses | **Not done.** `catch` is a statement each backend implements; adding a type test means three engine changes plus a user error hierarchy to test against. |
| User error hierarchy (a base `Error` type) | **Not done** — it is the prerequisite for typed catch. |
| Null safety (`T?`, non-null `T`) | **Not done.** `?.`/`??` from Stage 2b cover the ergonomics; a non-null type is a semantic change that deserves its own stage with a strictness switch. |
| `?` in a closure | Works (it returns from the closure, which is the correct meaning), but it is untested — the tests use plain functions. |

The remaining error-model work is the *typed* half: an error hierarchy, typed
catch, and then a non-null type. That is a coherent stage of its own, and it is
where the three-engine cost finally bites, because `catch` is not desugarable.
