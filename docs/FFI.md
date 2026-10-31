# Native FFI (`extern fn`)

Calling C from a Spidr program. **AOT only** — the tree interpreter and the
bytecode VM reject `extern fn` outright, because they link nothing.

```spidr
extern fn c_add(a: int, b: int) -> int;
extern fn c_hello(name: string) -> string;

fn main() {
    println(c_add(20, 22));      // 42
    println(c_hello("Ada"));     // Hello, Ada!
    return 0;
}
```

```console
$ clang -c cmath.c -o cmath.o
$ spidr build app.matrix -o app --link-lib cmath.o
$ ./app
42
Hello, Ada!
```

## The declaration

```spidr
extern fn <c_symbol>(<name>: <type>, …) -> <type>;
```

The name is the **C symbol**, verbatim — no `_` prefixes, no mangling, no
aliasing. A `;` ends the declaration; there is no body. `extern` is not a
reserved word, so existing programs that use it as an identifier are unaffected.

## The four types

Only these cross the boundary:

| Spidr | C | Notes |
| --- | --- | --- |
| `int` | `int64_t` / `long long` | 64-bit; not `int` |
| `float` | `double` | a Spidr `int` argument is accepted and widened |
| `bool` | `bool` / `_Bool` (1 byte) | `0` is false, anything else is true |
| `string` | `const char*` | UTF-8, NUL-terminated |

Anything else — a list, a map, a class, a closure — is refused **at build time**
with the type named. Structs, callbacks and GC values across the boundary are
deliberately out of scope: they need a calling convention and an ownership story
that v1 does not define.

A parameter with no type is also an error: C has no untyped parameters.

## What the marshalling costs

Every call copies its string arguments into temporary C buffers and frees them
as soon as the call returns. A string **returned** from C is copied into a Spidr
string and the C buffer is *not* freed — the callee may have returned a literal
or a static buffer, and freeing it would be a double free.

That means: a C function must not retain a pointer to a string argument after it
returns. If it needs to keep the data, copy it on the C side.

## Linking

`--link-lib` is repeatable. A bare name becomes `-l<name>` on Unix and
`<name>.lib` under MSVC; a path (or anything ending in `.a`, `.lib`, `.so`,
`.dylib`) is passed to the linker as given.

```console
$ spidr build app.matrix -o app --link-lib m          # -lm
$ spidr build app.matrix -o app --link-lib ../lib/libfoo.a
```

A symbol nothing defines is a **link error**, so the build fails and no binary
is produced:

```console
$ spidr build app.matrix -o app
error[usage]: linking failed (…/link.exe):
spid3r_app.o : error LNK2019: unresolved external symbol no_such_symbol …
```

That is the point of resolving at build time rather than at run time: a missing
symbol can never reach a user as a crash.

## On the script engines

```console
$ spidr run app.matrix
error[usage]: extern fn 'c_add' needs a native executable: the tree interpreter
cannot call C. Build with `spidr build`.
$ echo $?
2
```

The rejection happens **before any statement runs**, on both engines, and exits
`2` (usage). The alternative — defining a body-less function — would return
`null` and fail somewhere unrelated, which is the one outcome worse than an
error. `docs/ENGINE_CONTRACT.md` explicitly permits a backend to reject a
construct it cannot implement.

## Trust

FFI is **tier T5**: trusted code. A C function runs in the process with full
authority — it can read files, open sockets, corrupt memory. There is no sandbox
and there cannot be one; the library is native machine code. Treat a linked
library exactly as you would treat linking it into any other program.

For untrusted third-party code, use a WASM plugin (`kind = "wasm"`) instead: it
has no imports, so it has no ambient authority to abuse.

## Not implemented

| Feature | Why not |
| --- | --- |
| Runtime loading (`dlopen`) | no stable ABI over `Value`; the AOT backend links at build time |
| Struct passing | needs layout agreement between two compilers |
| Callbacks into Spidr | a Spidr closure is a GC'd `Value`, not a C function pointer |
| C++ symbols | name mangling is compiler-specific |
| Varargs | no way to express them in the declaration |
| Calling C from `spidr run` | the tree/VM link nothing — by design |

## Files

| Path | What |
| --- | --- |
| `src/parser/mod.rs` | parses `extern fn …;` (two-token lookahead, `extern` is not a keyword) |
| `src/native/ir.rs` | `declare_externs` + `compile_extern_call` — the marshalling |
| `src/native/rt.rs` | `spid3r_to_i64` / `to_f64` / `cstr` / `value_from_cstr` |
| `src/native/link.rs` | `--link-lib`, and the platform-specific link argument |
| `tests/ffi.rs` | round trip, engine rejection, missing symbol, bad type |
