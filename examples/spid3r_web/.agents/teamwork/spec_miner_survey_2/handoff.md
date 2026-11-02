# Spid3r Language Specification & Modern Feature Survey Handoff Report

**Agent**: `teamwork_spec_miner` (Survey 2)  
**Date**: 2026-11-01  
**Working Directory**: `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\spec_miner_survey_2`  
**Target Project**: `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web`  
**Authoritative Language Source**: `C:\Users\needyamin\OneDrive\Desktop\spid3r` (grammar, parser, AST, semantic, lower, stdlib, CLI, tests)

---

## 1. Observations

### 1.1 Specification & Core Architecture Observations
1. **Grammar & Reserved Keywords (`docs/GRAMMAR.md`, `docs/KEYWORDS.md`)**:
   - `docs/KEYWORDS.md:7-51`: Core keywords include `fn`, `let`, `const`, `if`, `elif`, `else`, `while`, `for`, `in`, `break`, `continue`, `return`, `true`, `false`, `null`, `and`, `or`, `not`, `import`, `try`, `catch`, `finally`, `throw`, `class`, `abstract`, `yield`, `interface`, `new`, `self`, `public`, `private`, `spawn`, `async`, `await`, `extends`, `implements`, `match`, `enum`, `super`, `as`.
   - `docs/GRAMMAR.md:8-12`: Program consists of `{ import } { declaration }`. All `import` statements **must** appear at the top of the file before any other declaration or statement; otherwise `src/parser/mod.rs:271` yields `error[parser]: import must appear before other declarations`.
   - `docs/GRAMMAR.md:10`, `src/parser/mod.rs:346-385`: Imports support three forms:
     - Flat: `import "module";` (imports all `public` declarations into file scope).
     - Namespace: `import "module" as alias;` (creates a dictionary namespace `alias`).
     - Selective: `import { A, B as C } from "module";` (selectively imports symbols with optional aliasing).
   - `src/semantic/mod.rs:1731-1734`: Declarations in imported modules are private by default. Accessing an unexported declaration throws `error[semantic]: <kind> '<name>' in module '<path>' is private (add 'public' to export it)`.

2. **OOP, Visibility, Inheritance, and Static Methods (`src/parser/mod.rs:697-830`, `tests/core_evolution_tests.rs:1-11, 322-348`)**:
   - `src/parser/mod.rs:723-728`: Class members without an explicit `public` keyword default to **`Vis::Private`**.
   - `src/parser/mod.rs:732-780`: Classes support `public static fn method(params) -> type { ... }` or `static fn` methods, callable directly on the class identifier `ClassName.method(...)` without instantiation.
   - `docs/GRAMMAR.md:241-249`, `src/ast/mod.rs:377-398`: Inheritance is expressed as `class Sub extends Super implements Iface1, Iface2`.
   - `super(args)` invokes the parent constructor; `super.method(args)` calls the parent implementation with `self`.
   - `src/parser/mod.rs:820`: Constructor shorthand `new(params) { ... }` is parsed identically to `fn new(params) { ... }`.
   - `tests/core_evolution_tests.rs:165-171`: Interfaces support default method implementations (`public interface Describable { fn title(self) -> string; fn badge(self) -> string { return "[" + self.title() + "]"; } }`).

3. **String Interpolation & Literals (`src/parser/mod.rs:2046-2049, 2413-2470`, `docs/GRAMMAR.md:197-204`)**:
   - Double-quoted strings support interpolation holes `"{name}"` or `"{obj.field}"` or arbitrary parsed expressions `"{len(xs)}"`. Holes compile to `"..." + string(expr) + "..."`.
   - Escaping interpolation braces: `"\{text\}"` renders literal `{text}` without interpolation (`tests/core_evolution_tests.rs:287`).
   - Raw string literals: `r"..."` and `r#"..."#` disable escape sequences and interpolation (`tests/core_evolution_tests.rs:288-292`).
   - Unicode escape sequences: `\u{1F600}` (1–6 hex digits, Rust-style) and `\uXXXX` (4 hex digits, JSON surrogate-pair style) (`tests/f7_unicode_escape.rs`).
   - Floats in strings: `"{:.2}".format(3.14159)` formats with 2 decimal places (`"3.14"`).

4. **Modern Ergonomic Syntax & Expressions (`src/parser/mod.rs:2079-2248`, `tests/stage2c_ergonomics.rs:1-132`)**:
   - Arrow closures: `x => expr`, `(a, b) => a + b`, `() => expr`, and block bodies `(a, b) => { return a + b; }`.
   - Map literals: `{ "key": val, other: 123 }` and `#{ "key": val }` desugar to `map("key", val, "other", 123)`.
   - List literals: `[1, 2, 3]` desugar to `list(1, 2, 3)`. Fresh mutable default parameters: `fn f(xs = [], cfg = {})` create a new list/map instance on every invocation.
   - Tuple literals: `(a, b, c)` and destructuring `let (a, b) = expr;`.
   - Two-variable loops: `for (k, v) in map` walks entry keys and values directly in insertion order.
   - Null-coalescing (`??`) and safe navigation (`?.`):
     - `a ?? b`: evaluates `b` only if `a` is `null` (`docs/GRAMMAR.md:205-208`).
     - `obj?.field`: evaluates to `null` if `obj == null`, or if `obj` is a `map` and the key is missing (`tests/f4_opt_field.rs`).
   - Result propagation operator `?`: `let x = fallible()?;` unwraps `ok(v)` or immediately returns `err(e)` / `none` from enclosing function.
   - Gradual types: `int`, `float`, `bool`, `string`, `null`, `any`, nullable types `T?` (`string?`), and union types `A | B` (`int | string`).
   - `list.join(sep)` / `vec.join(sep)` auto-stringifies every element using canonical display form (`tests/f6_list_join.rs`).

5. **Error Handling & Exceptions (`src/parser/mod.rs:1461-1514`, `tests/error_tests.rs:1-100`)**:
   - `try { ... } catch (e) { ... } finally { ... }`.
   - In `src/parser/mod.rs:1477`: `catch` **strictly requires parentheses**: `catch (err)`. Writing `catch e {` generates a syntax error: `"catch requires parentheses: catch (err)"`.
   - At least one `catch` or `finally` block is mandatory (`src/parser/mod.rs:1499`).
   - Thrown values can be any type (`throw "error"`, `throw map("code", 400)`). Uncaught exceptions surface as `error[runtime]: uncaught throw`.

6. **Networking, HTTP, and Security Primitives (`docs/STANDARD_LIBRARY.md:204-244`)**:
   - Sockets: `tcp_listen`, `tcp_accept`, `tcp_accept_timeout`, `tcp_connect`, `tcp_read`, `tcp_read_exact`, `tcp_write`, `tcp_write_bytes`, `tcp_close`.
   - `tcp_accept_timeout(listener, ms)` returns `null` on timeout, allowing graceful server loop shutdown via `shutdown_requested()` / `reset_shutdown()`.
   - HTTP: `http_read_request`, `http_write_response`, `http_fetch`, `http_body_json`, `url_decode`, `url_encode`.
   - Server runner: `serve_listener(listener, handler, opts, max_conns)` from `stdlib/http.matrix`.
   - Cryptography & Entropy:
     - `random_bytes(n)`: OS CSPRNG (`/dev/urandom`, `RtlGenRandom`) returning `ok(bytes)`. **Authoritative requirement for security-sensitive tokens, cookies, session IDs.**
     - `random_int(lo, hi)` is xorshift PRNG seeded from clock and stack address; **unsuitable for cryptography or tokens**.
     - `uuid_v4()` in `stdlib/uuid.matrix` uses `random_bytes`.
     - `bcrypt_hash(pwd)` and `bcrypt_verify(pwd, hash)` for secure password hashing.
     - `secure_compare(a, b)` for constant-time string comparison.
   - Multipart & Binary I/O:
     - HTTP requests carry `body_bytes` alongside `body`. `req["body"]` is lossy UTF-8 (replaces invalid bytes with U+FFFD); binary uploads (images, PDFs) **must** read `req["body_bytes"]` (`docs/STANDARD_LIBRARY.md:557-559`).

7. **CLI `magic` Execution Protocol (`src/cli.rs:791-895`, `tests/magic_tests.rs`)**:
   - Running `spid3r magic <subcommand>` looks up `magic` or `magic.matrix` in the current directory or parents.
   - Automatically sets working directory to project root and prepends project root to `SPID3R_PATH`.
   - Directly executes the project's `magic` script.
   - Subcommand dispatch in `examples/spid3r_web/magic` forwards arguments to `ConsoleKernel.handle(args())`.

### 1.2 Observations of `examples/spid3r_web` Current State
1. **`spid3r magic test` Failure**:
   - Command run: `spid3r.exe magic test` in `examples/spid3r_web`.
   - Output: `Unknown command: 'test'. Run 'spid3r magic help' for a list of available commands.` (Exit code 1).
   - Reason: `examples/spid3r_web/framework/Console/ConsoleKernel.matrix:35-89` handles `serve`, `route:list`, `migrate`, `migrate:rollback`, `migrate:status`, `db:seed`, `make:controller`, `make:model`, `make:migration`, but has no handler for `test`.
   - Existing test runner: `examples/spid3r_web/tests/run_all.matrix` runs all 6 test suites when invoked with `spid3r run tests/run_all.matrix`, but is not wired into `magic test`.
   - In `tests/run_all.matrix:9-21`, the test runner attempts to locate `"target/release/spid3r.exe"` with fallback paths, which creates environment dependencies.

2. **Obsolete Language Constructs in `spid3r_web`**:
   - **String Concatenation Overuse**: Excessive `+` string concatenations (e.g. `sql = "SELECT " + self.select_cols.join(", ") + " FROM " + self.table_name;`) instead of string interpolation (`"SELECT {self.select_cols.join(\", \")} FROM {self.table_name}"`).
   - **Constructor Verbosity**: Everywhere uses `map("key", val)` and `list(...)` instead of modern map literals `{ "key": val }` and list literals `[...]`.
   - **Manual Map Iteration**: `for k in map.keys() { let v = map[k]; }` instead of modern entry destructuring `for (k, v) in map`.
   - **Verbose Function Closures**: `fn(req) { return ...; }` instead of concise arrow closures `(req) => ...`.
   - **Manual Result Unwrapping**: Extensive manual `if res.is_ok() { let val = res.unwrap(); } else { return err; }` instead of error propagation `res?`.
   - **Missing Class Inheritance**: In `app/Models/User.matrix` and `app/Models/Post.matrix`, classes use composition `self.model = new Model(...)` and separate static classes (`UserStatic`, `UserModel = new UserStatic()`) instead of modern `class User extends Model` with `public static fn`.
   - **Insecure Session Tokens**: `framework/Auth/Auth.matrix:36` generates session tokens using `"sess_" + string(unix_ms()) + "_" + string(user["id"])` — entirely predictable timestamps rather than CSPRNG `random_bytes` / `uuid_v4()`.

---

## 2. Logic Chain

1. **Premise 1 (Spec Compliance)**: The authoritative Spid3r core repository defines modern language semantics in `docs/GRAMMAR.md`, `docs/STANDARD_LIBRARY.md`, `src/parser/mod.rs`, `src/lower/mod.rs`, and `tests/core_evolution_tests.rs`.
2. **Premise 2 (Acceptance Criteria)**: `ORIGINAL_REQUEST.md` mandates:
   - Modernize and harden core subsystems (Active Record ORM, router, middleware, `.bd.html` templates, authentication, `magic` CLI runner) with Spid3r's latest language features and conventions.
   - Eliminate obsolete constructs and ensure clean error handling and zero platform-specific execution artifacts.
   - `spid3r magic test` must run cleanly and pass all tests successfully.
   - `cargo test --test magic_tests` and `cargo test --test web_stack_tests` must continue to pass with 0 failures.
3. **Inference 1 (Test Runner Integration)**: Because `spid3r magic test` currently reports `Unknown command: 'test'`, `ConsoleKernel.matrix` must be extended with a `test` command that invokes the test suite runner directly within the Spid3r runtime without relying on external binary search paths.
4. **Inference 2 (OOP & Static Methods Modernization)**: Because Spid3r natively supports `extends`, `super()`, `super.method()`, and `public static fn`, the ActiveRecord subsystem (`Model.matrix`, `User.matrix`, `Post.matrix`) should transition from awkward companion helper instances (`UserModel = new UserStatic()`) to idiomatic model inheritance: `class User extends Model` with `public static fn find(id)`, `public static fn all()`, etc.
5. **Inference 3 (Syntactic Ergonomics Modernization)**: Replacing legacy `list(...)`, `map(...)`, `for k in m.keys()`, and string concatenation chains with `[...]`, `{...}`, `for (k, v) in m`, and interpolation `"{...}"` aligns the codebase with modern production Spid3r idioms.
6. **Inference 4 (Security & Hardening)**: Replacing timestamp-based tokens (`"sess_" + string(unix_ms()) + "_" + string(user["id"])`) with CSPRNG `uuid_v4()` or `hex_encode(random_bytes(32).unwrap())` satisfies the production readiness and security contract.

---

## 3. Caveats

1. **Parser Precedence of `import`**: Any new files or modernized files must place all `import` statements before any statement or declaration; otherwise, compilation fails with `import must appear before other declarations`.
2. **Default Visibility in Classes**: Class members without explicit `public` are private by default. When modernizing classes, all methods and fields intended for external access must retain or add the `public` modifier.
3. **Catch Syntax**: `catch (e)` requires parentheses; omitting them causes a parse error.
4. **Gradual Types**: Type annotations (`name: string`, `-> int`, `string?`, `int | string`) are gradual and checked during semantic analysis; using them enhances code clarity and catches compile-time defects without runtime overhead.
5. **Lossy String Body for Binary Uploads**: File uploads and binary payloads must read `req["body_bytes"]` instead of `req["body"]`.

---

## 4. Conclusion

The Spid3r language has evolved significantly with modern ergonomic constructs:
- **Core Evolution**: Cross-module class inheritance (`extends`), `super()`, default interface methods, `public static fn`, selective imports (`import { A, B as C } from "mod"`), arrow closures (`(x) => expr`), map/list literals (`{}`/`[]`), fresh mutable default parameters, escaped interpolation `\{...\}`, raw strings `r"..."`, null-safe navigation `?.`, null coalescing `??`, Result propagation `?`, and gradual typing (`T?`, `A | B`).
- **Legacy Framework Deficiencies**: The framework in `examples/spid3r_web` currently exhibits legacy patterns: manual string concatenation, `map(...)`/`list(...)` verbose constructors, composition workarounds for missing static methods, predictable session token generation, and a missing `magic test` command.
- **Modernization Blueprint**: Modernizing `spid3r_web` requires:
  1. Implementing `command_test` in `ConsoleKernel.matrix` to support `spid3r magic test`.
  2. Refactoring models (`Model.matrix`, `User.matrix`, `Post.matrix`) to use `class Model` base with `public static fn` and `super()`.
  3. Adopting string interpolation `"{...}"`, map literals `{...}`, list literals `[...]`, and arrow closures `(req) => ...`.
  4. Hardening `Auth.matrix` session token generation using CSPRNG `random_bytes` / `uuid_v4()`.
  5. Modernizing route definitions in `routes/web.matrix` and `routes/api.matrix` using arrow functions.

---

## 5. Verification Method

To verify these observations and specifications independently:

1. **Verify Core Repository Test Suites**:
   ```powershell
   cd C:\Users\needyamin\OneDrive\Desktop\spid3r
   cargo test --test magic_tests
   cargo test --test web_stack_tests
   cargo test --test core_evolution_tests
   ```
2. **Verify CLI Magic Help & Test Execution**:
   ```powershell
   cd C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web
   C:\Users\needyamin\OneDrive\Desktop\spid3r\target\debug\spid3r.exe magic help
   C:\Users\needyamin\OneDrive\Desktop\spid3r\target\debug\spid3r.exe run tests/run_all.matrix
   ```
3. **Verify Semantic & Parser Constraints**:
   Inspect `src/parser/mod.rs` (lines 271, 723-780, 1461-1514, 2079-2248) and `src/semantic/mod.rs` (lines 1706-1755).

---

## Features Discovered

| # | Category | Feature | Description | Inputs | Outputs | Error Behavior | Discovered Via |
|---|----------|---------|-------------|--------|---------|----------------|----------------|
| 1 | Module System | Selective Import | `import { A, B as C } from "mod";` imports specific symbols with optional aliasing. | Quoted path, identifier list | Binds selected names | `error[semantic]` if symbol missing or private | `src/parser/mod.rs:347-385`, `tests/core_evolution_tests.rs:194` |
| 2 | Module System | Import Placement Rule | All `import` statements must precede all other statements/declarations in a file. | Top of file | Program AST | `error[parser]: import must appear before other declarations` | `src/parser/mod.rs:270-272` |
| 3 | Module System | Default Privacy | Declarations in modules without `public` are private and cannot be imported. | `fn`, `class`, `let`, `const` | Exported symbol or private | `error[semantic]: <kind> '<name>' in module '<mod>' is private` | `src/semantic/mod.rs:1731-1734` |
| 4 | OOP | Class Inheritance | `class Sub extends Super` inherits fields and methods across modules. | Superclass identifier | Subclass definition | `error[semantic]` if superclass unknown | `src/parser/mod.rs:702`, `tests/core_evolution_tests.rs:136` |
| 5 | OOP | Super Constructor & Method | `super(args)` calls parent constructor; `super.method(args)` calls parent method. | Constructor / method args | Super call dispatch | `error[semantic]` if used outside class or wrong arity | `docs/GRAMMAR.md:241-249`, `src/ast/mod.rs:384-398` |
| 6 | OOP | Static Class Methods | `public static fn name(params)` defines methods callable as `ClassName.name(...)`. | Method params | Return value | `error[semantic]` if called on non-class | `src/parser/mod.rs:732-780`, `tests/core_evolution_tests.rs:322` |
| 7 | OOP | Constructor Shorthand | `new(params) { ... }` inside a class body declares the class constructor. | Parameters, body block | Instance initialization | Arity checks at `new ClassName(...)` | `src/parser/mod.rs:819-830`, `docs/GRAMMAR.md:235` |
| 8 | OOP | Member Default Privacy | Class fields and methods without `public` are private by default. | Member declaration | Private member | Calling private member outside class fails | `src/parser/mod.rs:723-728`, `docs/GRAMMAR.md:238-240` |
| 9 | OOP | Default Interface Methods | `interface I { fn f(self) -> T { ... } }` provides optional default implementations. | Method signature + body | Interface definition | Subclass can override or inherit | `tests/core_evolution_tests.rs:166-171` |
| 10 | Syntax | Map Literals | `{ "key": val, other: 123 }` or `#{ ... }` constructs an insertion-ordered `map`. | Key-value pairs | `map` instance | Parse error if malformed | `src/parser/mod.rs:2198-2225`, `tests/core_evolution_tests.rs:260` |
| 11 | Syntax | List Literals | `[1, 2, 3]` constructs a resizable `list`. | Comma-separated expressions | `list` instance | Parse error on unclosed `]` | `src/parser/mod.rs:2228-2247` |
| 12 | Syntax | Tuple Literals & Destructuring | `(a, b)` tuple literal; `let (x, y) = expr;` unpacks elements in a single evaluation. | Tuple / List expression | Bound variables | `error[runtime]` if unpacking wrong length | `docs/GRAMMAR.md:217`, `tests/stage2c_ergonomics.rs:43-69` |
| 13 | Syntax | Arrow Closures | `x => expr`, `(a, b) => expr`, `() => expr`, and `(a: int) -> int => { ... }`. | Parameter list, expression / block | Function closure | Arity mismatch on invocation | `src/parser/mod.rs:2079-2195`, `tests/core_evolution_tests.rs:268` |
| 14 | Syntax | String Interpolation | `"hello {name} {1 + 2}"` evaluates embedded expressions to strings. | Expression in `{}` | Interpolated `string` | Parse error if hole syntax is invalid expression | `src/parser/mod.rs:2046, 2413`, `docs/GRAMMAR.md:197` |
| 15 | Syntax | Escaped Interpolation & Raw Strings | `"\{literal\}"` escapes braces; `r"C:\dir\{no_interp}\n"` raw string literal. | Literal string | Unescaped string | Lexer error on unclosed string | `tests/core_evolution_tests.rs:286-292` |
| 16 | Syntax | Unicode Escapes | `\u{1F600}` (1–6 hex digits) and `\uXXXX` (JSON surrogate pairs). | Hex digits | Unicode character | `error[lexer]` on invalid codepoints/surrogates | `docs/GRAMMAR.md:187-196`, `tests/f7_unicode_escape.rs` |
| 17 | Syntax | Null-Coalescing | `a ?? b`: evaluates and returns `b` only if `a == null`. | Left and right expressions | Result value | None (evaluates right side lazily) | `docs/GRAMMAR.md:205-208`, `src/lower/mod.rs:17` |
| 18 | Syntax | Safe Navigation | `obj?.field`: evaluates to `null` if receiver is null or receiver is a map missing the key. | Object / map, field name | Field value or `null` | Throws if receiver is non-null primitive or class missing field | `tests/f4_opt_field.rs`, `src/ast/mod.rs:415` |
| 19 | Syntax | Result Propagation | `expr?`: unwraps `ok(v)` or immediately returns `err(e)` / `none` from enclosing function. | Result or Option | Unwrapped value | Returns early from caller if failure | `src/ast/mod.rs:426-428`, `docs/GRAMMAR.md:125` |
| 20 | Syntax | Value-Position If | `let x = if (c) { 1 } else { 2 };` returns branch expression value without trailing `;`. | Condition, value blocks | Evaluated branch value | Lowered into temporary variable | `docs/GRAMMAR.md:139-141`, `src/lower/mod.rs:15` |
| 21 | Syntax | Pattern Matching | `match (expr) { Pattern [if guard] => body, ... }` matches variants, literals, bindings. | Scrutinee expression | Matching arm result | `error[runtime]: unmatched value` if non-exhaustive | `docs/GRAMMAR.md:143-146`, `tests/stage3_match.rs` |
| 22 | Syntax | Mutable Default Parameters | `fn f(xs = [], cfg = {})` provides fresh mutable list/map instances on each call. | Default literal `[]` or `{}` | Fresh collection instance | Lowered to runtime allocation per call | `tests/core_evolution_tests.rs:248-256, 294-301` |
| 23 | Types | Gradual Nullable & Union | `T?` (`string?`) and `A | B` (`int | string`) validated at compile time. | Type annotations | Checked types | `error[semantic]: type mismatch` | `src/semantic/ty.rs`, `tests/core_evolution_tests.rs:351` |
| 24 | Control Flow | Map Iteration | `for (k, v) in map { ... }` iterates over insertion-ordered key and value pairs. | `map` instance | Loop variable bindings | Runtime error if target not map | `docs/GRAMMAR.md:218`, `tests/stage2c_ergonomics.rs:73-82` |
| 25 | Control Flow | Strict Catch Syntax | `try { ... } catch (e) { ... } finally { ... }` requires parentheses around catch variable. | Exception identifier | Error value | `error[parser]: catch requires parentheses: catch (err)` | `src/parser/mod.rs:1464-1478` |
| 26 | Collections | Auto-Stringifying Join | `list.join(sep)` / `vec.join(sep)` auto-converts non-string elements using display form. | Separator string | Joined `string` | `error[runtime]` if separator is not string | `docs/STANDARD_LIBRARY.md:903-908`, `tests/f6_list_join.rs` |
| 27 | Collections | Sorting with Comparator | `xs.sort_by(cmp)` (in-place) and `xs.sorted_by(cmp)` (new list) with stable ordering. | Comparator function `(a, b)` | Null or sorted list | Throws if comparator returns invalid type or NaN | `docs/DATA_STRUCTURES.md:41-96` |
| 28 | Networking | Timeout TCP Accept | `tcp_accept_timeout(listener, timeout_ms)` performs bounded accept for clean shutdown. | Listener handle, ms timeout | `ok(conn)`, `null` (timeout), `err` | Network OS error | `docs/STANDARD_LIBRARY.md:212`, `tests/web_stack_tests.rs:626` |
| 29 | Networking | Binary Body Handling | HTTP server request map provides `body_bytes` for loss-free binary upload processing. | HTTP request map | `bytes` buffer | Lossy fallback if using string `body` | `docs/STANDARD_LIBRARY.md:557-559`, `tests/web_stack_tests.rs:199` |
| 30 | Security | Cryptographic CSPRNG | `random_bytes(n)` generates cryptographically secure pseudorandom bytes from OS. | Byte length `int` | `ok(bytes)` / `err` | OS entropy failure | `docs/STANDARD_LIBRARY.md:199` |
| 31 | Security | Password Hashing | `bcrypt_hash(pwd, cost=10)` and `bcrypt_verify(pwd, hash)` with constant-time verification. | Password string, hash string | Hash string or `bool` | Clamped cost (4..=16) | `docs/STANDARD_LIBRARY.md:126-127` |
| 32 | Security | Constant-Time Equality | `secure_compare(a, b)` prevents timing side-channel attacks on secrets and hashes. | Strings or bytes | `bool` | None | `docs/STANDARD_LIBRARY.md:130` |
| 33 | CLI | Magic Subcommand Dispatch | `spid3r magic <subcommand>` discovers project root, sets `SPID3R_PATH`, runs `magic`. | Subcommand & args | Exit code | `error[usage]` if no `magic` file found | `src/cli.rs:791-895`, `tests/magic_tests.rs` |

---

## Edge Cases

| # | Feature | Input | Observed Behavior |
|---|---------|-------|-------------------|
| 1 | Catch Clause Syntax | `try { ... } catch e { ... }` (omitting parentheses) | Parser fails with `error[parser]: catch requires parentheses: catch (err)`. Parentheses `catch (e)` are strictly mandatory. |
| 2 | Import Placement | Declaration or `let` before `import "mod";` | Parser fails with `error[parser]: import must appear before other declarations`. All imports must precede declarations. |
| 3 | Class Member Visibility | Calling method declared without `public` prefix from outside class | Calling non-public method from another class or top-level fails as private access error. |
| 4 | Map OptField with `??` | `map("a", 1)?.missing ?? "default"` | Evaluates cleanly to `"default"`. Missing key in `?.` on map returns `null`, then `??` supplies fallback. |
| 5 | Instance OptField | `class Box { public let x; } let b = new Box(1); b?.missing` | Throws `error[runtime]: instance of Box has no field 'missing'`. `?.` protects maps and null receivers, not class fields. |
| 6 | Primitive OptField | `(5)?.x` | Throws `error[runtime]: int has no field 'x'`. Non-fieldable types still raise runtime error. |
| 7 | List Join Types | `[1, true, null, [2, 3]].join("-")` | Output: `"1-true-null-[2, 3]"`. Non-string elements auto-convert to canonical display text. |
| 8 | List Join Separator | `[1, 2].join(5)` | Throws `error[runtime]: list.join expects a string, found int`. Separator must strictly be a string. |
| 9 | Null-Coalescing `none` | `none ?? "fallback"` | Evaluates to `none` (not `"fallback"`), because `none` is an Option variant, not `null`. Use `.unwrap_or("fallback")` for options. |
| 10 | String Interpolation Format | `"{:.2}".format(3.14159)` | Output: `"3.14"`. Uses tie-to-even rounding up to precision 15. Greater than 15 throws error. |
| 11 | Bare PascalCase in Match | `enum Color { Red, Green } match (c) { Red => 1, Green => 2 }` | Matches variant `Red()`, does NOT act as a wildcard bind variable `Red`. |
| 12 | Non-Callable Spawn | `spawn 42;` | Throws `error[runtime]: spawn expects a function, found int`. |
| 13 | HTTP Binary Payload Loss | Uploading PNG image and reading `req["body"]` | Bytes `0x80`, `0xFE`, `0xFF` are corrupted or converted to `U+FFFD`. Must read `req["body_bytes"]`. |
| 14 | TCP Accept Stalling | Server loop calling `tcp_accept(l)` during shutdown | Loop stalls indefinitely on Windows because accept blocks without interruption. Must use `tcp_accept_timeout(l, ms)`. |
| 15 | Magic CLI Missing Command | `spid3r magic test` without `ConsoleKernel` handler | Fails with exit code 1: `Unknown command: 'test'. Run 'spid3r magic help' for a list of available commands.` |
