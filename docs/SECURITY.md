# Security model

This document states what Spid3r does and does not protect you from. It is
deliberately short and concrete: if a claim here is wrong, that is a bug worth
reporting.

## The trust model: scripts are code, not data

**A `.matrix` program runs with the full authority of the process that starts
it.** It can read and write files, start child processes, open sockets and read
environment variables — the same authority the launcher has. There is no
sandbox, no capability check and no permission prompt by default.

That is the right default for the common case (a program you wrote, run by you)
and the wrong one for the case that needs care: **running a script you did not
write**. If the script comes from a user, a plugin author or a network fetch,
treat it exactly as you would treat a native binary from the same source.

The comparison to make is not "like a config file" but "like `sh`".

## Embedding: `Engine::deny`

`spid3r_lang::embed` is the supported way to host Spid3r inside a Rust
application. Since 2026-10-28 it can remove the built-in natives a script needs
for the four broad capabilities:

```rust
use spid3r_lang::embed::{Capability, Engine};

// Everything: filesystem, processes, network, environment.
let engine = Engine::new().deny_all();

// Or pick:
let engine = Engine::new()
    .deny(Capability::FileSystem)
    .deny(Capability::Process);
```

A denied native still exists as a name — calling it raises a **catchable**
`capability denied: '<name>' is not available to this script`, so a script can
degrade gracefully instead of crashing.

### What `deny` is not

It is a policy filter over the built-in natives, **not a sandbox**:

- A host function you registered yourself (`Engine::register`) keeps whatever
  authority you gave it. `deny` cannot take that back.
- Denying `FileSystem` does not stop a script from reaching the filesystem
  through *your* native, or through a `.matrix` module that wraps one.
- It is not a resource limit. A script that is allowed to loop can still loop;
  see "Denial of service" below.
- Module *loading* is not restricted: a script can `import "http"` even when
  `Network` is denied — every call in that module fails at the native boundary.

If you need a real sandbox, run the script in a separate process with OS-level
isolation (containers, seccomp, job objects) and keep the host interface narrow.

## Concurrency rules

Spid3r has real threads: `spawn` runs tasks on an OS thread pool, and the HTTP
server serves each connection on its own task.

- **Shared mutable state must be guarded with `mutex()`.** This is a language
  rule, not a suggestion. Two tasks incrementing the same captured variable lose
  updates on every engine — the write is atomic, the read-modify-write is not:

  ```
  // lost updates under concurrency, on tree, vm and native alike
  let n = 0;
  let f = fn() { n = n + 1; return n; };
  ```

  The fix is the same as anywhere else: `let n = mutex(0);` and hold the lock
  across the whole read-modify-write.

- **Stdlib parsers are reentrant.** `xml_parse`, `yaml_parse` and `toml_parse`
  keep their cursor in a per-call object, so concurrent parses cannot corrupt
  each other (fixed 2026-10-28).

- **Closure environments are memory-safe to share.** Captured variables live in
  atomic cells, so a closure moved to another thread cannot be torn. The
  *logical* race above still applies.

## Untrusted input

The stdlib is written assuming its input is hostile:

| Surface | Bound |
|---|---|
| XML / YAML / TOML | Nesting depth capped; DTDs, anchors and tags rejected rather than half-parsed; malformed input is a catchable error |
| Regex | One quantifier ≤ `{1000}`; whole pattern ≤ 500 000 nodes, checked before allocation (`((a{1000}){1000}){1000}` is rejected in <1 ms) |
| `render_template` | Substituted values are never re-scanned, so user data cannot become template syntax; `{{ key }}` escapes HTML. `{{{ key }}}` does **not** — do not use the raw form with user data |
| `json_parse` | Depth-capped, iterative |
| `parse_query` / `parse_form` | No size limit of their own — cap the request body at the server |

### Denial of service

- **Execution budget (bounded work).** `Engine::budget(steps)` (library) and
  `spid3r run --budget N` (CLI) cap the work one run may do. The tree engine
  charges one unit per statement and per loop iteration; the VM charges in
  batches at its instruction poll. Exhaustion raises the **fatal** error
  `execution budget exhausted`, which `try`/`catch` cannot intercept — a
  runaway script cannot swallow the stop and keep running. The allowance is
  per run and shared by the tasks the script spawns, and module
  initialization draws from it too. The native AOT backend does not enforce a
  budget (a counter in generated machine code is not free): run untrusted
  code on the tree or VM engine.
- **No memory cap.** A script can allocate until the host runs out. Cap the
  process's memory (cgroup, job object, `ulimit`) or the request size at the
  server.

For a server, use the HTTP layer's own limits (`max_body_bytes`,
`max_connections`, idle timeouts) and rate-limit at the edge.

## Cryptography

- `random_bytes(n)` reads the OS CSPRNG (`/dev/urandom`, `RtlGenRandom`) and is
  the only source for tokens. On a platform with no entropy source it returns an
  error rather than falling back.
- `random_int` / `random_float` / `random_normal` are **not** cryptographic: a
  thread-local xorshift seeded from the clock. Fine for sampling and jitter,
  never for a token.
- `uuid_v4()` uses `random_bytes` and throws if entropy is unavailable (fixed
  2026-10-28). On `wasm32` there is no OS entropy yet, so `uuid_v4` throws there.
- `jwt_verify` compares signatures in constant time and enforces `exp`/`nbf`
  with a 60-second skew allowance (fixed 2026-10-28). It verifies `HS256` only;
  it does not check `alg` against a policy, so do not feed it tokens from an
  issuer you do not control.
- Cookies default to `SameSite=Lax`; `Secure` is opt-in because a `Secure`
  cookie set over plain http is dropped by the browser (fixed 2026-10-28).

## Packaged applications (`build --embed` / `--bundle-runtime`)

A single-file app carries its program, assets and (optionally) the runtime
inside its own executable. That adds five surfaces worth naming.

- **The payload is obfuscated, not encrypted.** The XOR key ships in the
  binary, so "no readable source" means "not visible to `strings`". For
  `--embed app.matrix` the source *is* in the file; anyone who can run the app
  can recover it. `--bytecode` avoids shipping text and keeps application
  literals out of the native image, but the VM is open source — treat it as an
  anti-casual-reading measure, not secrecy.
- **The payload parser treats its input as hostile.** Entry lengths are
  attacker-controlled bytes appended to a file, so the reader uses checked
  arithmetic (a `u64::MAX` length is a clean error, not a panic), rejects
  absolute paths, `..` segments, backslashes and duplicate names, and never
  writes an entry to disk. `gui --payload` serves entries from memory over
  `spid3r://app/`; the `--html` page on disk is served with a canonicalised
  root check, so `../` and `%2e%2e/` cannot escape it.
- **`__spid3r_runtime/` is reserved.** `run_payload()` extracts and executes
  exactly `__spid3r_runtime/spid3r[.exe]`, whether or not `--bundle-runtime`
  was used, so `--embed` refuses any other entry under that prefix — otherwise
  a build could point the launcher at an arbitrary embedded executable.
- **A payload that cannot be parsed is a hard error, not a fallback.** If a
  launcher finds a payload and cannot read it — corrupted body, impossible
  lengths, unsafe entry names, a trailer that claims more bytes than the file
  holds — it logs the reason and refuses to start anything. It does *not* fall
  through to `%SPID3R_EXE%`, the per-user install or `PATH`, because that would
  let a damaged file redirect the app to a different binary. The fallback is
  kept for the one case it is meant for: an executable that carries no payload
  at all (a loose/dev build). See "Known gaps" for the case this does not
  cover.
- **The extracted runtime is verified with SHA-256 before it runs.**
  `--bundle-runtime` writes `spid3r.exe` to
  `%LOCALAPPDATA%\Spid3r\runtime\<sha256>\`, and an existing file is re-hashed
  (not merely size-checked) before use: a same-size impostor placed by another
  process running as the same user is replaced, not executed. The directory key
  and the check are both the SHA-256 of the runtime bytes; a non-cryptographic
  hash has no business doing integrity work even where forging one is
  impractical. The write is atomic (temporary file + rename), so concurrent
  launches cannot run a half-written executable.
  A copy that cannot be *read* (an antivirus scanner holding it) is unverifiable,
  which is not the same as wrong: it is left in place, never executed, and the
  runtime is unpacked into a fresh directory beside it — so a temporary lock
  neither destroys the cache nor gets an unverified binary run. This is cache
  integrity, not a privilege boundary: anything that can write to
  `%LOCALAPPDATA%` already runs as the user, and can also modify the app's own
  executable.

Packaged apps append failures to `%LOCALAPPDATA%\Spid3r\logs\spid3r.log`
(`SPID3R_LOG=0` disables, `SPID3R_LOG_FILE` relocates it). Messages are the
toolchain's own diagnostics; they can quote file paths and program output, so
treat the log as user data, not as something to attach to a public report
without reading it.

## The toolchain's own DLL loading

`spid3r build` calls into LLVM through a **delay-loaded** `LLVM-C.dll`, so the
DLL is resolved at the first AOT call rather than at startup. The Windows loader
searches the application directory and the *current directory* before `PATH`, so
without a mitigation a `LLVM-C.dll` dropped beside `spid3r.exe` — or into a
project directory a developer just cloned — would run its `DllMain` inside the
toolchain. That is the one confirmed code-execution vector in this pipeline, and
it affects the **developer's toolchain**, not packaged apps (an app runs bytecode
on the VM and never touches the AOT backend).

The mitigation is a preload, not a global search-order change: before the first
LLVM call, `cmd_build` locates the genuine DLL (explicit overrides → standard
LLVM install locations → `PATH` **excluding the current directory and the
executable's directory** → the executable's directory last) and loads that exact
path with `LoadLibraryExW(..., LOAD_WITH_ALTERED_SEARCH_PATH)`. The delay-load
helper's later `LoadLibrary("LLVM-C.dll")` then returns the already-loaded
module, so the planted copy is never reached. `SetDefaultDllDirectories` is
deliberately *not* used: `db_open` loads `sqlite3.dll` by name and users keep it
on `PATH`, so a process-wide restriction would break database support to fix one
DLL.

Residual, and a deployment decision rather than a code defect: running the
toolchain **elevated**, or building **untrusted pull requests in CI**, widens
this class of problem beyond what a per-user tool can defend against. Do neither,
or run those builds in a container with a fixed toolchain.

## Known gaps

These are known and not yet fixed. They are listed so nobody assumes otherwise:

- **No capability model below `Engine::deny`.** There is no per-path filesystem
  scope, no per-host network allowlist, no memory ceiling.
- **The native (LLVM) backend is less exercised than the tree and VM engines.**
  Treat it as beta for hostile input.
- **The GUI/webview backend** (`spid3r gui`) exposes whatever the webview
  exposes; it has only been smoke-tested.
- **No CSRF middleware.** The web stack has no `mw_csrf`; add one, or use
  `SameSite` cookies plus an origin check.
- **Sessions are client-side.** `web.SessionStore` signs the data; the client can
  read it. Do not put secrets in a session payload.
- **A hand-written launcher can still opt into the lenient fallback.** Stripping
  a whole payload trailer is indistinguishable from never having one, so
  `run_payload()` treats "no payload here" as the loose/dev case and falls back
  to `%SPID3R_EXE%` / the per-user install / `PATH`. The generated stub
  (`spid3r build --bytecode`) now calls **`run_payload_strict()`**, where a
  missing payload is fatal, which covers the documented single-file path. A
  launcher you write yourself and compile with `--embed` only keeps the lenient
  behaviour — call `run_payload_strict()` in that case if the build is meant to
  be self-contained. Corrupt-but-present payloads are rejected either way.

## Reporting

Security issues should go to the maintainers privately rather than in a public
issue. Include the version (`spid3r version`), the platform, and the smallest
program that reproduces the problem.
