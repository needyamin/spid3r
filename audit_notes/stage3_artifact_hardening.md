# Stage 3 — artifact and dependency hardening (AUDIT_2026-10-20 §6)

Date: 2026-10-21 · Scope: A-5 (git transport → RCE), A-8 (VM panics on crafted
`.matrixc`), A-11 (unverified WebView2 download in the build), plus the two
lockfile gaps the audit lists under "also noted" (lossy/partial source hashing,
git pins not enforced on re-install).

Baseline for every "before" claim: a `git worktree` at `0d87fee` (the Stage-2
commit) built separately and driven with the same programs/tests.

## 1. What was fixed

| ID | Defect | Fix |
|---|---|---|
| A-5 | A manifest-controlled `git = "ext::<cmd>"` URL ran `<cmd>` during `spid3r install` (CVE-2022-25912 class); `--` does not neutralize it | `pkg::validate_git_url` whitelist: `https://`, `ssh://`, scp-like `user@host:path`. `http://`/`git://` need `SPID3R_ALLOW_INSECURE_GIT=1`, local paths/`file://` need `SPID3R_ALLOW_LOCAL_GIT_DEPS=1`, **`ext::` is rejected unconditionally**. Also rejects leading `-`, whitespace/control chars, ssh hosts starting with `-` (CVE-2017-1000117 class), and unknown schemes |
| A-13 | `spider.lock` recorded a commit but re-install re-resolved the branch tip (pinning was cosmetic) | `read_locked_sources` + `locked_commit`: a re-install checks out the recorded commit and verifies `HEAD` equals it; `spid3r install --update` drops the pin. A missing commit fails with an actionable message |
| A-13 | Vendor hashing covered only `*.matrix` and decoded contents with `from_utf8_lossy` (two different binaries hashed the same; non-`.matrix` assets were neither copied nor hashed) | `collect_files` takes every file under `src/`; `hash_files` hashes **raw bytes** (`path` NUL `contents` NUL, sorted). `tree_hash` and `vendor_dep` share it |
| A-8 | `code[ip]` unchecked in the VM loop; `span_at` blind-indexed `inst_spans[lo-1]`; deserializer read offsets/spans counts independently; no trailing-byte check; unbounded deserialize recursion; `starts.contains` made validation quadratic | Bounds-checked fetch (clean VM error), `span_at` uses `get`, validator requires `inst_offsets.len() == inst_spans.len()` + strictly increasing + inside the code, rejects trailing bytes, caps nesting at `MAX_DESERIALIZE_DEPTH = 128`, requires a terminator (`return`/`throw`/`jump`) and a non-empty code section, and binary-searches the instruction starts |
| A-11 | A plain `cargo build --release` (gui default) downloaded the WebView2 bootstrapper with no checksum, accepted any non-empty file as usable, and cached it in the source tree | `build_support/webview2.rs`: opt-in (`SPID3R_FETCH_WEBVIEW2=1`), pinned SHA-256 (+ `SPID3R_WEBVIEW2_SHA256` override) and minimum size, download to `.part`, verify, then rename; any failure deletes the file and the installer is built without it. No network access by default |

Supporting change: the SHA-256 implementation moved to `src/pkg/sha256.rs` and is
`#[path]`-included by `build.rs`, so the package layer and the installer verify
with the *same* code instead of two copies that could drift.

## 2. Evidence (before → after)

```
case                                        before (0d87fee)        after
git = "ext::sh -c 'touch pwned.txt'"        install runs the cmd    rejected before any clone
git = "http://…"                            accepted (no integrity) rejected, names the opt-in
git = "ftp://…"                             accepted as a path      rejected (unsupported scheme)
re-install after upstream commit            vendor moves to the tip vendor stays at the locked commit
install --update                            n/a                     moves to the new tip
src/blob.bin 80 81 82 vs 90 91 92           identical tree_hash     different tree_hash
src/table.txt vendored?                     no (only *.matrix)      yes
.matrixc + 4 trailing bytes                 accepted                rejected ("trailing byte(s)")
inst_offsets=2, inst_spans=1                panics in span_at       rejected ("instruction tables disagree")
chunk ending in Pop                         accepted → panic later  rejected ("fall-through")
200 nested function chunks                  unbounded recursion     rejected ("depth limit")
50k jumps (debug build)                     ~5.6 s                  < 0.1 s
exec on a corrupt .matrixc                  panic (exit 101/abort)  exit 1 + error[vm] diagnostic
WebView2 download                           implicit, unverified    opt-in, SHA-256, fail closed
```

## 3. Regression tests added

* `tests/artifact_hardening.rs` (11): trailing bytes, instruction-table
  mismatch (plus `span_at` totality), fall-through/empty chunk, nesting depth,
  non-quadratic jump validation (50k jumps < 5 s), `exec` on four corrupt
  `.matrixc` shapes (must exit 1 with `error[`, never `panicked`), and the
  WebView2 policy (opt-in parsing, checksum override validation, fail-closed
  verification of tampered/partial/missing files, `ensure_with` never accepting
  an unverified file).
* `tests/pkg_tests.rs` (+5): `ext::` rejected with no command run, plaintext and
  unknown schemes rejected, locked commit honored on re-install and moved by
  `--update`, non-`.matrix`/binary assets vendored and hashed as raw bytes, and
  the full transport-policy matrix (accepts https/ssh/scp/IPv6/ext:: in a path;
  rejects ext::, http, git, file, ftp, leading `-`, ssh-host `-` tricks, local
  paths, Windows drives, whitespace).
* Existing suites updated: `Project::run` opts into local git transports
  (`SPID3R_ALLOW_LOCAL_GIT_DEPS=1`) because the tests build local repositories.

Validation: copied into the pre-fix worktree, the A-8 tests **fail**
(trailing/tables/fall-through/quadratic) and the A-5 tests fail (ext:: runs the
command; the re-install follows the tip). A test that cannot fail was not
accepted as evidence.

## 4. Gates (executed, sequential)

| Gate | Result |
|---|---|
| `cargo build --no-default-features --all-targets` | OK |
| `cargo clippy --no-default-features --all-targets -- -D warnings` | 0 warnings (build script included) |
| `cargo test --no-default-features` | **481 passed, 0 failed** |
| `cargo test --no-default-features --features gui --test gui_tests` | 10/10 |
| `cargo test --no-default-features --features native --test native_tests` | 5/5 |
| `spid3r test` (tree == vm == exec) | 25 ok, 0 failed |
| `spid3r fmt --check` | all unchanged, exit 0 |
| `cargo build --target wasm32-unknown-unknown --release --lib --no-default-features` | OK |

## 5. Deliberate decisions and remaining limits

* **`ext::` is rejected even with an opt-in.** The audit suggested making it
  opt-in alongside `file://`; a manifest that can run a shell command during
  `install` has no safe default, and the person who sets the variable is the one
  about to clone the hostile manifest. Documented in the function comment.
* **`sha256.txt` compatibility break.** The hash construction changed (raw bytes,
  all files). Registry packages that published a `sha256.txt` generated by the
  old code will now fail the check; regenerate with `spid3r pack` (it prints the
  new `sha256(tree)`). Documented in `docs/REGISTRY.md`. No in-repo fixture
  depended on the old value (the registry test generates it at runtime).
* **The installer is still built by `cargo build --release`.** A-11's
  supply-chain surprise was the *download*; that is gone. The nested release
  build remains (the installer needs a finished `spid3r.exe`), and
  `SPID3R_NO_INSTALLER=1` still disables the whole step. Making installer
  generation opt-in would change the project's release workflow, which is
  outside this stage's scope.
* **The WebView2 network path is not exercised by the suite.** The decision logic
  (`verify_file`, `ensure_with`, opt-in/checksum parsing) is tested directly; the
  15-line `curl.exe` invocation is not (no network in CI, and a release installer
  build is a multi-minute side-effecting job). The pinned digest was computed
  from the real bootstrapper (1,876,736 bytes,
  `f414de01c4680f0bb0694d1ea7803c6968f3a81bf6bad01b89bfc65dce14c2a2`); Microsoft
  rotates the fwlink, and a rotation now fails closed with instructions instead
  of silently bundling a different file.
* **`validate_chunk` still recurses over nested chunks** (bounded by the
  deserializer's depth cap for any file that came through `deserialize_vmfunc`;
  a chunk built programmatically in-process is trusted).
* **Path dependencies are not pinned.** `path+…` sources are local by
  definition; the lock records the origin and a content hash, but there is no
  commit to pin.
