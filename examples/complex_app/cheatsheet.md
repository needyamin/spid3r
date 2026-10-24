# Spid3r Cheatsheet — verified vs examples/, tests/, docs/

*Research-verified reference produced while building `nebula_core.matrix`
(2026-10-25). Every claim was checked against example programs, tests, or
docs — two corrections discovered during the field test are marked ⚠.*

## 0. Running
```
spid3r run app.matrix              # tree engine (default)
spid3r run --engine vm app.matrix  # bytecode VM
spid3r build app.matrix -o app     # LLVM native binary (AOT)
spid3r compile app.matrix -o appc  # serialized bytecode (header "SPID3R"), run via `spid3r exec`
spid3r test                        # differential suite: tree == vm == exec
```
Source files: `.matrix`. Exit 0 success / non-zero error. Engines tree, vm, native/AOT, wasm
must produce byte-identical stdout + same stderr phase/message + same exit code
(docs/ENGINE_CONTRACT.md).

## 1. Program structure
```
// examples/hello.matrix
fn main() { print("hello"); return 0; }
```
- `//` line comments; `/* */` block (non-nesting). Statements end `;`. Blocks `{ }`.
- Top-level globals legal: `let g = 10;` (tests/engine_matrix.rs global_let).

## 2. Variables & types (examples/variables.matrix)
```
let count = 10; let ratio = 0.5; let name = "spid3r"; let ready = true;
const LIMIT = 3;                  // constant
count = count + 5;                // no `mut` keyword — plain assignment
let b: int = 2;                   // optional annotations
let a: option<int> = some(5); let c: result<int> = ok(1);
```
- Primitives: int(i64), float(f64, literals like 1.5e3), bool, string, null.
- Same-scope shadowing = semantic error `'x' is already declared in this scope`.
- Truthiness: false, null, none, "", 0 are falsy (examples/conditions.matrix).
- int + float -> float. No implicit string coercion (see §14).

## 3. Printing — exact formats (byte-parity)  (tests/engine_matrix.rs literals/float_format)
```
println(42);   // 42        println(1.5); // 1.5
println(4.0);  // 4.0       integral floats always print one decimal
println(0.1+0.2); // 0.30000000000000004
println(true); // true      println(null); // null
println([1,2]); // [1,2]    println(set(1,2)); // set{1,2}
println(map("a",1)); // map{a: 1}   ("__"-prefixed keys hidden)
println((1,"two",3.5)); // (1, two, 3.5)     println(some(5)); // some(5) / none / ok(1) / err(bad)
print("a","b"); // a b      multiple args joined by ONE space
```
- print: no newline; println: newline; eprint: stderr.
- string(x) == what print shows — ONE shared display fn (src/value/mod.rs display_value).
- Instance prints as `Dog {name: Rex, noise: woof}`.

## 4. Control flow
```
if score >= 90 { } elif score >= 80 { } else { }   // parens optional; `elif` OR `else if`
while i < 3 { }                                    // parens optional
for x in [10, 20, 30] { }                          // for-in over list
for (k, v) in m { println("{k}={v}"); }            // map pairs, INSERTION order (for_kv test)
for let k = 0; k < 5; k = k + 1 { if k==1 {continue;} if k==3 {break;} }  // `let` REQUIRED
let x = if (n > 90) { "A" } else { "B" };          // if is an expression; no else -> null
match (s) {                     // tests/engine_matrix.rs enum_match/match_guards
    Circle(r) => 3 * r * r,     // enum pattern
    Rect(w, h) if w == h => "square",   // guard; failed guard falls to NEXT arm
    Empty() => 0,
    0 => "zero",                // literal pattern
    x => "many ({x})",          // binding pattern; `_` wildcard allowed
};
enum Shape { Empty, Circle(radius), Rect(w, h) }   // variants are constructors: Circle(2)
```
- Enum match must be EXHAUSTIVE (else compile error). Patterns nest: Dot(At(x, y)).
- Destructure: `let (a, b) = pair();  let (x, y, z) = [1, 2, 3];`

## 5. Functions (examples/functions.matrix)
```
fn add(a, b) { return a + b; }                    // param annotations OK: a: int
fn fact(n) { if n <= 1 { return 1; } return n * fact(n - 1); }   // recursion
let base = 100;                                   // closures capture defining scope
let adder = fn(x) { return x + base; };
let f = add; f(1, 2);                             // first-class fns
fn greet(name, greeting = "Hello", punct = "!")   // defaults: trailing-only, constants
connect(host: "b.io", secure: true);              // named args: direct calls + ctors ONLY
let p = new Point(y: 5);                          // (method calls reject them)
```
- Return annotation optional: `fn speak(self) -> string`.
- Runtime arity error is catchable & engine-identical: `method greet expects 1 argument(s), got 2`.

## 6. Classes (examples/class.matrix, interface.matrix; tests/super_tests.rs)
```
class Animal {
    public let name;               // fields declared, not initialized
    private let noise;             // private: outside use = semantic error
    fn new(name, noise) { self.name = name; self.noise = noise; }   // ctor = fn new
    public fn speak(self) -> string { return self.name + " says " + self.noise; }
}                                  // `self` is an EXPLICIT receiver param
class Dog extends Animal {
    fn new(name) { super(name, "woof"); }    // super(args); parent methods super.m(args)
}                                            // ALL engines incl. AOT (tests/super_tests.rs)
let d = new Dog("Rex");            // `new Dog("Rex")` and `Dog("Rex")` both construct
interface Named { fn name(self) -> string; }
class User extends Object implements Named { }    // class Team implements Named { }
```
- Dispatch is by method name through any variable. `type_of(x)` -> "int"/"map"/...
- NO `instanceof` operator. Class-in-function rejected on all engines.

## 7. Dunders — the COMPLETE set (tests/engine_matrix.rs)
```
class Vec2 {
    public let x; public let y;
    fn new(x, y) { self.x = x; self.y = y; }
    public fn __add__(self, o) { return Vec2(self.x + o.x, self.y + o.y); }
    public fn __sub__(self, o) { ... }  __mul__ __div__ __neg__ similarly
}
```
- Full set: `__add__ __sub__ __mul__ __div__ __neg__` + reflected `__radd__ __rsub__ __rmul__ __rdiv__`
  (reflected fires when the LEFT operand has no overload: `10 + Num(5)`).
- NO `__str__`/`__eq__`/`__lt__` — printing and == cannot be customized.
- On maps: `m["__add__"] = fn(a, b) { return mk(a["v"] + b["v"]); };` then `mk(1) + mk(2)`.
- Missing dunder -> `error[runtime]: cannot add instance and instance`.

## 8. Strings (tests/engine_matrix.rs string_interp/string_methods; examples/higher_order)
```
"hi {name}, {n} years"       // hole = {ident(.ident)*}; call holes also evaluate (verified live)
"user: {u.name} ({u.age})"   // field access OK
"{not a hole}"               // stays literal; "{x }" stays literal
"{{ user }}"                 // braces are NEVER escaped — non-holes pass through
"a={} b={}".format(1, "two") // a=1 b=two
"ab" + "cd"                  // strings only; "user-" + string(id) for numbers
"abc"[0]; "abc"[-1]          // "a"; "c" (negative from end; 1-char string)
"abc".slice(1, 3)            // "bc";  "ab".repeat(2) // abab
"k=v".split("=").join("|")   // k|v
```
- Methods: len upper lower trim trim_left trim_right split join replace contains
  starts_with ends_with index_of slice reverse repeat pad_left pad_right format.
- ord(s)/chr(n); for-in over a string yields 1-char strings (docs).
- ⚠ Field test: there is NO `\uXXXX`/`\u{...}` escape in literals — `unknown escape '\u'`
  (report.html F-7). Non-ASCII must be pasted as raw UTF-8.

## 9. Collections (examples/list|map|set|heap.matrix)
```
let xs = list(1, 2, 3);  // or [1, 2, 3]
xs.push(40); xs.pop(); xs.get(0); xs[-1]; xs.len(); xs.contains(30);
xs.remove_at(1); xs.reverse(); xs.sort(); xs.clear(); xs.is_empty();
ys.sorted(); ys.reversed();     // NON-mutating copies
let m = map("a", 1, "b", 2);    // INSERTION-ORDERED
m["k"] = 7; m.k; m?.k;          // index-assign; dot reads keys; null-safe dot
m.get("prod"); m.contains("k"); m.has("k"); m.keys(); m.values(); m.items();
m.delete("a"); m.remove("a"); m.len();
m.get("missing")                // THROWS — not null!
```
- map keys: string/int/bool OK; float keys REJECTED.
- FIXED (F-4, 2026-10-25): `m?.missing` reads `null` on every engine now —
  `?.` guards a null receiver AND a missing map key, so `m?.k ?? fallback`
  works. Everything else is unchanged: `m.get("missing")` and plain
  `m.missing` still throw, an instance without the field still throws.
- `list.join` needs all-string elements: `join needs strings, found int` (report.html F-6).
- `let s = set(1, 2, 3);  s.union(b); s.intersection(b); s.difference(b); s.remove(1);`
- `let h = heap("min"); h.push(5); h.peek(); h.pop();   // NO tie-order promise`
- `let pq = priority_queue(); pq.push("urgent", 0); pq.pop();  // ties: arrival order (stable)`
- `let t = (1, "two", 3.5);        // t[1], len(t); (7,) 1-elem; () empty; (5) is just int`
- No comparator sort documented anywhere; sort()/sorted() verified on numbers only.
- Also: stack() queue() deque() ring_buffer(4) bytes("abc") bst() graph("undirected")
  matrix(2, 3, 0.0) vector(list(1.0, 2.0)) ref(1).

## 10. Errors (examples/error_handling.matrix)
```
try { print(10 / 0); } catch(error) { eprint("caught: " + error); } finally { println("done"); }
throw "boom";                          // throw ANY value; crosses catch unchanged
let m = map(); m["code"] = 7; throw m; // catch(e) { e["code"] } — value, not string
let r = parse_age("42");               // ok(n)/err(msg); r.is_ok(); r.is_err(); r.unwrap()
none_val.unwrap();                     // unwrap on none/err THROWS (catchable)
some(2).map(fn(x){...}); none().ok_or("m"); err("e").map_err(fn(e){...});
ok(3).and_then(fn(x){...});
fn quarter(n) { let h = half(n)?; return ok(h / 2); }   // `?` propagates err
```
- Uncaught: `error[runtime]: <msg>`, exit 1; AOT adds `uncaught throw (thrown: …)`.
- `?.` null-safe read (null receiver AND missing map key -> null, F-4), `??`
  null coalescing (PHP semantics: `none ?? 5` -> none), `?` result
  propagation — three different operators.
- FIXED (F-9, 2026-10-25): the stack-overflow error is catchable on the NATIVE
  backend now (was: abort exit 70).

## 11. Concurrency (examples/tasks|channel.matrix; tests/deadlock_tests.rs)
```
let t1 = spawn fn() { return 1; };     // closure -> task
let t2 = spawn work;                   // named fn, NO parens
t1.join();                             // joins, returns value
async fn join_two(a, b) { return await a + await b; }   // await ONLY inside async fn
async fn await_channel(ch) { return await ch; }         // await ch == recv one message
worker.cancel(); worker.join();        // join after cancel THROWS "task cancelled"
worker.status();                       // "cancelled" ...
let ch = channel();  ch.send("done");  ch.recv();       // recv blocks (parks, no spin)
let mu = mutex(0);
let v = mu.lock();                     // lock() RETURNS the guarded value — NOT a guard
mu.unlock();                           // must unlock manually; same-task relock = catchable
sleep_ms(5); yield(); task_count();
```
- spawn = OS thread pool; scheduling nondeterministic on EVERY engine — order via channels.

## 12. GC (docs/GC.md)
```
gc_collect(); gc_live_cells();         // builtins
let w = weak(ref(42)); deref(w);       // -> value or null once collected
```
- Refcounting frees acyclic garbage eagerly; cycles auto-collected (mark-sweep).
- Auto-collection at statement boundaries ON by default; `SPID3R_GC=0` disables.
- gc_collect() refuses (catchable) while another thread runs Spid3r code.
- ⚠ Live-cell counts are ENGINE-INTERNAL — the same workload measured
  380 (tree) / 159 (vm) / 300462 (native); on native (arena model) gc_collect()
  reclaims nothing by construction. Adopted policy (ENGINE_CONTRACT exemption #4,
  docs/GC.md): counts are exempt from byte-parity; the portable facts (both
  builtins return int, collection is safe, live data survives) are pinned by
  tests/gc_portability.rs. Never print raw counts.

## 13. Stdlib: globals (NO import) vs modules (docs/STANDARD_LIBRARY.md)
Globals include: print println eprint read_line len abs sqrt pow min max floor ceil trunc
round sign clamp exp ln log10 log2 sin cos tan asin acos atan atan2 hypot is_nan is_inf
nan() inf() float_or int float string bool ord chr random_int(lo,hi) random_float
random_normal random_bytes(n) sleep_ms now now_ms unix_time unix_ms datetime_parts
format_datetime read_file write_file file_exists file_delete dir_create dir_remove
read_bytes write_bytes parse_csv read_csv write_csv save_npy load_npy
json_parse json_write (NOT json_encode/json_decode) msgpack_* cbor_* pb_*
base64_* hex_* base32_* sha256 hmac_sha256 pbkdf2_sha256 bcrypt_* argon2 scrypt
secure_compare crc32 adler32 deflate inflate gzip_* zlib_* zip_* png_* wav_* pdf_*
tcp_* udp_* ws_* http_read_request http_write_response http_fetch url_encode url_decode
regex_match regex_find regex_find_all regex_replace  (. * + ? ^ $ [...] \d \w \s | ())
env(name)->option args() spawn_process exec_capture yield task_count shutdown_*
type_of gc_collect gc_live_cells weak deref
29 first-party modules in ./stdlib/ DO need import:
```
import "math_util";              // resolves next to the file
import "dataframe" as df; import "stats" as st; import "ml" as ml; import "bigint" as bi;
```
- Modules export `public fn`/`public enum`; rest private; no main needed.
- Big numbers: `100n` bigint literals (module: to_string/divmod/neg/abs/from_int/cmp);
  rational module; decimal/complex value types exist (they print via display strings).
- ⚠ Field test: `import "x" as alias` is REJECTED by the native backend
  ("not supported by the native backend yet") — use plain `import "x";` for AOT builds
  (report.html F-2).

## 14. Numeric semantics (src/interpreter/mod.rs; native rt test)
```
7 / 2      // 3    int/int TRUNCATES (C-style, NOT Python)
-7 % 3     // -1   modulo sign follows the DIVIDEND
1 / 0      // error "division by zero"; 1 % 0 -> "modulo by zero"
2.5 * 2    // 5.0  int promotes to float
9223372036854775807 + 1  // runtime error "integer overflow: ..." (+ - * checked; no wrap,
                         // no auto-bigint — use 100n)
"a" + 1    // runtime error "cannot add string and int"
6 & 3 == 2 // error! == binds tighter than &: 6 & (3 == 2)
1 << 2 + 1 // 8    + binds tighter than <<
false and boom()  // short-circuits
1 << 64    // error "shl: shift amount 64 is out of range (0..63)"  (verified live)
```
- `&&`/`||` do NOT exist — the operators are the keywords `and`/`or` (verified live).
- `int("42.9")` throws `int('42.9'): not an integer` — strict parse (verified live).

## 15. Engines (docs/ENGINE_CONTRACT.md)
- All backends: byte-identical stdout, same stderr phase+first line, same exit code.
  Unsupported construct => compile-time `error[usage]`, never silent divergence.
- Only permitted gaps: (1) AOT diagnostics lack spans/snippets/stack; uncaught native adds
  `uncaught throw (thrown: …)`; (2) call-depth trigger point may shift slightly; (3) spawn
  scheduling nondeterministic everywhere.
- engine_matrix rows marked aot:false (semantic-error rows, async_await, global_let,
  collection_literals, string_methods) are pinned tree/vm only — no AOT guarantee implied.
- docs/GRAMMAR.md claims AOT lacks `super` — OUTDATED; super_tests.rs covers compiled AOT.

## 16. Gotchas & rough edges
1. `7 / 2 == 3` — int division truncates; no separate float-division operator.
2. `"a" + 1` is a runtime error — use string(x), `"{x}"`.
3. Integral floats print with .0 (2.5*2 -> "5.0"); 0.1+0.2 -> "0.30000000000000004".
4. `map.get(missing)` THROWS; guard with contains/has — or read with `?.`:
   `m?.k ?? fallback` DOES protect missing keys now (F-4 fixed 2026-10-25).
5. mutex.lock() returns the value, not a guard — always pair with unlock(); same-task
   relock = catchable deadlock error, mutex stays usable.
6. `??` is null-only: `none ?? 5` -> none (options are not null).
7. `==` binds tighter than `&`: parenthesize bitwise expressions.
8. Enum match must be exhaustive at compile time; failed guards fall through; `Red()` is
   the canonical variant pattern (bare `Red =>` also matches).
9. Named args rejected on method calls; positional-after-named rejected.
10. await only inside async fn; join() works anywhere.
11. No `__str__`/`__eq__` dunders — display and == are fixed; instance shows `Name {field: v}`.
12. heap() tie order unspecified — use priority_queue for stable ties.
13. No comparator sort documented; sort()/sorted() verified on numbers only — hand-roll
    merge sort with a comparator closure (see nebula_core.matrix).
14. AOT: async/await, global let, and some collection/string method rows are not in the AOT
    conformance subset; class-in-function rejected everywhere; `import as` rejected (F-2).
15. gc_collect() refuses while other threads run Spid3r code (concurrent programs don't
    collect until quiescent); live-cell counts are engine-internal (F-3).
16. Regex has NO bounded-repeat quantifiers: `{n}`/`{n,m}` silently match literal braces
    (F-1) — use `+`/`?` expansions.
17. Trailing commas: allowed in match arms, rejected in call arguments (F-5).
