# Spid3r tooling / packaging / security audit

Repo: `C:\Users\needyamin\OneDrive\Desktop\GaliLang` (Windows host)
Scope: `src/cli.rs`, `src/modules/mod.rs`, `src/pkg/mod.rs`, `src/fmt.rs`, `src/main.rs`, `src/lib.rs`, `build.rs`, `difftest.sh`, `tools/gate.sh`, `tools/native_diff.sh`, `.gitignore`; `docs/REGISTRY.md` and `docs/BUILDING.md` as intent. Read-only audit: no build, no test run, nothing modified under `src/`, `tests/`, `examples/`, `docs/`.

Baseline used: the task brief's CLI contract (exit 0 success / 1 user-program error / 2 usage error), the module-system doc comment in `src/modules/mod.rs:1-3`, `docs/REGISTRY.md` (registry layout, constraints, `sha256.txt`), `docs/BUILDING.md` (feature behavior).

Result: **2 critical, 4 major, 8 minor**, plus notes. All findings are code-verified (cited lines); nothing was executed, so runtime behavior is inferred from the code paths, not observed.

---

## Must fix (6)

### T1 [critical] Package/dependency names are used as filesystem paths unvalidated — `install` can delete the project root
**Where:** `src/pkg/mod.rs:254-303` (dependency key taken verbatim, `deps.push` at 302), `:724-727` (`let dest = root.join("vendor").join(name); ... remove_dir_all(&dest)`), `:895` (`root.join(".spid3r/git").join(name)` for git deps), `:543` (`dist/<name>-<version>.tar`), `src/cli.rs:354` (`target/<name>.matrixc`), `src/pkg/mod.rs:567,953` (`root.join(member)`).

**What breaks:** A dependency key such as `..` or `../x` survives the manifest parser and is joined straight onto `vendor/`. `vendor_dep` then calls `remove_dir_all` on that path. With key `..`, `dest` is `<root>/vendor/..` = the project root, and `spid3r install` recursively deletes the project's contents. This is triggered by cloning and installing any untrusted project — the manifest is attacker-controlled input. The same missing validation lets git checkouts, dist tarballs and `.matrixc` outputs be written outside their intended directories via `..` in a dep/package name. (Names with `/` or `\` also escape: `a/b` writes into a nested dir.)

**Repro:**
```toml
# spider.toml in a freshly cloned "victim" repo
[package]
name = "victim"
version = "0.1.0"

[dependencies]
".." = { path = "../innocent" }   # ../innocent must be a valid package (has src/)
```
```sh
cd victim && spid3r install
# dest = <root>/vendor/..  ->  remove_dir_all deletes the project root's contents
```

**Fix direction:** Validate every identifier that becomes a path component at parse time (dependency name, `[package] name`, workspace member): reject `.`/`..`, `/`, `\`, `:`, control chars; allow `[A-Za-z0-9_.-]` only. Additionally assert `dest.starts_with(root.join("vendor"))` before `remove_dir_all`.

### T2 [critical] `unpack` path guard misses Windows drive-letter paths — arbitrary file write from an archive
**Where:** `src/pkg/mod.rs:603` (`if name.contains("..") || name.starts_with('/') || name.starts_with('\\')`), `:606` (`let target = dest.join(&name)`), `:611` (`fs::write`).

**What breaks:** On Windows, `Path::join` with a path that has a drive prefix (`C:/...`, `C:...`) discards `dest` entirely. The guard only rejects `..`, leading `/` and `\`, so an archive entry named `C:/Users/<user>/AppData/Roaming/.../x.matrix` is written to that absolute location instead of under the unpack destination. Any `:`-containing entry (NTFS alternate data streams, drive-relative `C:foo`) also bypasses the intent. `unpack` is the command users run on archives they did not create, so this is an arbitrary-write primitive at user privilege.

**Repro:**
```python
# build a hostile archive (any tar writer works; read_tar only needs a ustar header)
import tarfile, io
with tarfile.open("evil.tar", "w") as t:
    data = b"// pwned\n"
    ti = tarfile.TarInfo("C:/Users/Public/spid3r-pwned.matrix")
    ti.size = len(data)
    t.addfile(ti, io.BytesIO(data))
```
```sh
spid3r unpack evil.tar out    # writes C:\Users\Public\spid3r-pwned.matrix
```

**Fix direction:** After `let target = dest.join(&name)`, canonicalize/normalize and require `target.starts_with(dest)`; explicitly reject entries containing `:` or a Windows prefix component (`Path::new(&name).components()` must contain only `Normal` components).

### T3 [major] Argument injection into `git` via manifest-controlled `url` / `rev`
**Where:** `src/pkg/mod.rs:903-905` (`git clone --quiet <url> <dir>`), `:911` (`git checkout --quiet <rev>`), `:926` (`Command::new("git")`).

**What breaks:** `url` and `rev` come from `spider.toml` and are passed as bare arguments. No shell is involved, but git parses a leading `-` as an option: a `url` of `--upload-pack=<cmd>` makes git execute `<cmd>` as the pack helper on transports that spawn it, `--config=...` / `-c` smuggle configuration into the clone, and a `rev` of `--orphan`-style values is consumed as a checkout option. A malicious manifest thus escalates from "data file" to "command/config injection into the developer's git".

**Repro:**
```toml
[dependencies]
evil = { git = "--upload-pack=calc.exe", rev = "main" }
```
```sh
spid3r install   # git clone --quiet --upload-pack=calc.exe <checkout-dir>
```

**Fix direction:** Insert `--` before positional arguments (`git clone --quiet -- <url> <dir>`, `git checkout --quiet -- <rev>`) and validate `url` against an allowlist of schemes (`https://`, `ssh://`, `git@`, local paths) and `rev` against `[A-Za-z0-9._/-]+`.

### T4 [major] `sha256.txt` is checked against a different hash than the one `pack` prints — the documented integrity workflow always fails
**Where:** `src/pkg/mod.rs:734-751` (tree hash: per-file `path NUL contents NUL` concatenation), `:761-769` (compare to `sha256.txt`), `:532-536` (`pack` hashes the **tar archive**: `sha256_hex(&data)`); `docs/REGISTRY.md` "sha256.txt … `spid3r pack` prints the hash you can paste here".

**What breaks:** `pack` prints the SHA-256 of the .tar bytes; the registry check compares `sha256.txt` against a hash over the vendored `src/` tree with a different construction (no tar headers, includes relative paths and NUL separators, excludes nothing else). For any real package the two values cannot match, so a packager following the docs produces a package that `install` rejects with `checksum mismatch`. The integrity feature is unusable as documented.

**Repro:**
```sh
cd mathlib && spid3r pack                 # prints "sha256: <H_archive>"
echo "<H_archive>" > dist/sha256.txt      # per docs: paste the printed hash
mkdir -p /reg/mathlib && cp -r . /reg/mathlib/1.0.0
export SPID3R_REGISTRY=/reg
cd ../app && spid3r install               # error: checksum mismatch (declares H_archive, tree hashes to H_tree)
```

**Fix direction:** Define one hash. Either have `pack` additionally compute and print the tree hash (`vendor_dep`'s construction, e.g. `sha256(tree): <hex>`) and tell packagers to use that for `sha256.txt`, or make the registry check hash the same canonical archive form `pack` emits.

### T5 [major] Module cache is keyed by the un-canonicalized path — one file can be imported twice as two instances
**Where:** `src/modules/mod.rs:2` ("Modules are cached by canonical path"), `:33-48` (`resolve` returns the first joined candidate verbatim; no `canonicalize` anywhere in the file), `:70` (cache lookup), `:75` (cycle check), `:160-161` (cache insert). The VM shares the same `ModuleCache` (`src/vm/mod.rs:107`).

**What breaks:** `import "lib/util"` and `import "./lib/util"` resolve to `lib/util.matrix` vs `./lib/util.matrix` — both `is_file()`, different map keys. The module body executes twice, its top-level state exists twice, and the importer's later import silently replaces the earlier bindings. On Windows, case-variant imports (`"Util"` / `"util"`) hit the same bug. Cycle detection is also spelling-sensitive: a cycle written with a different spelling executes the module body twice before the cycle is reported. This contradicts the module's own doc comment and the hunt's "same file gets different instances" case.

**Repro:**
```matrix
// lib/util.matrix
println("util init");          // top-level side effect
public fn twice(x) { return x * 2; }
```
```matrix
// main.matrix
import "lib/util";
import "./lib/util";           // same file, different cache key
fn main() { println(twice(21)); return 0; }
```
`spid3r run main.matrix` prints `util init` twice.

**Fix direction:** Canonicalize `resolved` (`std::fs::canonicalize`, falling back to the raw path only on error) before using it as the `loaded` / `in_progress` key; do the same in `public_names_of` so the semantic pass and runtime agree.

### T6 [major] `spid3r init <new-dir>` writes the package name "app"
**Where:** `src/cli.rs:748-762` (`cmd_init`: name derived from `path.canonicalize()` with `unwrap_or_else(|| "app")` **before** `pkg::init` creates the directory), `src/pkg/mod.rs:1090-1102` (`init` creates `dir` afterwards).

**What breaks:** Canonicalizing a path that does not exist yet fails, so every fresh `spid3r init myproj` writes `name = "app"` into `spider.toml` (and therefore names `dist/app.matrixc`, `dist/app-0.1.0.tar`). Only `init` into an already-existing directory gets the right name.

**Repro:**
```sh
spid3r init myproj
grep '^name' myproj/spider.toml   # name = "app"
```

**Fix direction:** Derive the name from the literal argument's `file_name()` (or create the directory first, then canonicalize); fall back to "app" only when the path has no final component.

---

## Worth considering (8)

### T7 [minor] `check` / `exec` silently swallow unknown options
**Where:** `src/cli.rs:396-402` (`single_file` filters out every `--*` argument), used at `:223` and `:376`.
**Issue:** `spid3r check --frobnicate f.matrix` and `spid3r exec --engine vm f.matrixc` exit 0 as if the flag were honored; `run` by contrast rejects unknown options. Typos in CI scripts pass silently.
**Fix direction:** Reject unrecognized `--` flags (share `cmd_run`'s option loop), and support `--` as end-of-options optionally.

### T8 [minor] `fmt` renders negative literals tight against the operator: `x = -1` → `x =-1`
**Where:** `src/fmt.rs:556-562` (`needs_space`: for `Minus`/`Plus`, space only if `ends_value(prev) || prev is a keyword`; `Eq`/`EqEq`/`>=` are none of those), single spacing site at `:322`.
**Issue:** With `prev = Eq`, no space is emitted before the sign, so `let x = -1;` becomes `let x =-1;` and `if x == -1` becomes `if x ==-1`. The rule's own doc comment (`:556-559`) gives `x = -1` / `== -1` as the expected spaced examples — code and doc disagree. Token preservation is unaffected (the safety net at `:356-378` still passes), so this is cosmetic but deterministic. Verified by code trace only; not executed.
**Fix direction:** Treat any binary operator / `=` as "space before the sign" (`space = !matches!(prev, LParen | LBracket | Comma | Dot | Minus | Plus | ...)`), or add the operator kinds to the spaced set.

### T9 [minor] Caret constraints for `^0.x` accept any higher 0.y — unlike ecosystem caret semantics
**Where:** `src/pkg/mod.rs:100-108` (`Caret(want) => v.major == want.major && (v.minor, v.patch) >= (want.minor, want.patch)`), resolution `:823-860` picks the highest match.
**Issue:** `^0.1.2` matches `0.9.0`; Cargo/npm treat `^0.1.2` as `>=0.1.2, <0.2.0` because 0.x minors are breaking. A project pinning `^0.1.2` silently upgrades to a breaking 0.9. `docs/REGISTRY.md` states the literal rule ("same major"), so this is documented, but the convention mismatch is a supply-chain surprise. Related: registry version dirs that are not `major.minor[.patch]` (e.g. `1.0.0-beta`) are silently skipped rather than reported.
**Fix direction:** Special-case `major == 0` (`^0.1.2` ⇒ `0.1.x`), or rename the operator / document the difference prominently.

### T10 [minor] `SPID3R_PATH` is split on both `;` and `:` — Windows drive letters produce phantom search dirs
**Where:** `src/modules/mod.rs:169` (`path.split(';').chain(path.split(':'))`).
**Issue:** `SPID3R_PATH=C:\spid3r\libs` yields the intended entry plus `C` (relative dir `<cwd>\C\...`) and `\spid3r\libs` (drive-root absolute). With the working directory on another drive, `\spid3r\libs` resolves on the *current* drive — an import can silently bind to a file at `D:\spid3r\libs\x.matrix` when `C:\spid3r\libs` lacks it. Real but niche: correct entries are searched first.
**Fix direction:** Use `std::env::split_paths` (platform-correct) instead of splitting on both separators.

### T11 [minor] Output-stream and exit-code inconsistencies around the documented 0/1/2 contract
**Where:** `src/cli.rs:895` (`fmt` per-file errors via `println!` → stdout), `:432-435` (missing `examples/` returns `Phase::Parser` → exit 1), `:1015` (analogous `bench` message uses `Phase::Usage` → exit 2), `:672` (`debug` quit returns 3).
**Issue:** Errors on stdout break `fmt --check 2>/dev/null` scripting; two "you are in the wrong directory" errors map to different exit codes (1 vs 2); `HELP` documents only 0/1/2 while `debug`'s quit path exits 3 (documented only in a code comment at `:660-662`).
**Fix direction:** `eprintln!` for errors; use `Phase::Usage` for environment/usage conditions; list exit 3 in the help text (or reuse 0 for a user-initiated quit).

### T12 [minor] `compile -o` can overwrite its own input file
**Where:** `src/cli.rs:354,369` (`out_path` accepted as-is; `fs::write(&out_path, bytes)`).
**Issue:** `spid3r compile app.matrix -o app.matrix` reads the source, then replaces it with bytecode; no guard compares `out_path` with the input. Also `default` output next to the source is fine, but `-o` pointing at any source module silently destroys it.
**Fix direction:** Refuse when `out_path` canonicalizes to the input path (or has a `.matrix` extension).

### T13 [minor] Predictable, never-cleaned temp file in `spid3r test`
**Where:** `src/cli.rs:457` (`std::env::temp_dir().join(format!("spid3r_test_{}.matrixc", name.replace('.', "_")))`), `:459`.
**Issue:** Two concurrent `spid3r test` runs of the same example race on the same path; on a shared `/tmp`, a pre-created file/symlink at that name is overwritten (CWE-377-style). The `.matrixc` is left behind after every run. Also `:455/456/459` call `.to_str().unwrap()`, so a non-UTF-8 path in `examples/` panics (exit 101) instead of a diagnostic.
**Fix direction:** Unique temp name per process (pid/nonce) under a private dir, delete after use; return a usage error instead of unwrapping non-UTF-8 paths.

### T14 [minor] Manifest parsing edges: escaped quote before `#`, UTF-8 BOM, duplicate `[package]` keys
**Where:** `src/pkg/mod.rs:221-227` (`strip_comment` toggles `in_str` on every `"`, ignoring `\"`), `:216-250` (no BOM handling), `:236-248` (`name`/`version`/`entry` last-wins silently).
**Issue:** `name = "a\"#b"` is truncated at the `#` and then fails with "expects a quoted string"; a UTF-8 BOM (Notepad default) makes line 1 fail with "expected `key = value`"; duplicate keys are silently overwritten while duplicate dependencies are rejected — inconsistent strictness.
**Fix direction:** Escape-aware comment stripping, strip a leading `\u{feff}`, and error on duplicate keys.

---

## Code-quality notes (max 3)

1. `src/cli.rs:538,542` — failure messages contain literal `//n` where `\n` was intended, so test-failure output reads `outputs differ://n--- tree ---`.
2. `src/fmt.rs:340-344` — both cleanup loops pop `' '`; the second is dead code (the first already removed all trailing spaces), and removing a trailing indent by popping single chars hides the intent.
3. `src/pkg/mod.rs:984-1000` vs `:1072-1088` (and `src/cli.rs:223/584` `has_main`/`has_main_fn`) — the constraint-vs-manifest-version check and the "does this file define main" parse are duplicated; divergence risk when one copy is changed.

---

## Observation log

- `write_tar` silently truncates entry names to 100 bytes (`src/pkg/mod.rs:414-415`) and `put_octal` truncates values beyond 8 GiB (`:440-447`): `pack` succeeds, `unpack` writes to a truncated path. No ustar prefix/base-256 support.
- `read_tar` (`:450-470`) ignores typeflag and the ustar `prefix` field and does not verify the header checksum — GNU-tar archives with long names misextract; non-regular entries are materialized as files.
- The unpack guard's `name.contains("..")` also rejects harmless names such as `a..b.matrix`.
- `vendor_dep` hashes `String::from_utf8_lossy(&bytes)` (`:748`): distinct non-UTF-8 contents collapse to the same replacement chars, weakening tamper detection for non-UTF-8 files.
- `collect_all_files` (`:522-535`) recurses through `is_dir()` without tracking visited dirs — a junction/symlink loop on Windows recurses until failure.
- `install_workspace` never increments `report.unchanged`, so a workspace install always reports every dep as changed.
- REPL `:reset` (`src/repl/mod.rs:40-44`) clears the `known` name list but not the environment, so old bindings remain live but become unreferenceable — "bindings cleared" is misleading.
- `difftest.sh:14` classifies examples with `grep -q "fn main"` (string search), diverging from the AST-based `has_main_fn` in the CLI; `OUT=/tmp/difftest` is a fixed shared path; `BIN=target/debug/spid3r.exe` hardcodes the Windows debug binary.
- `tools/native_diff.sh` writes AOT binaries to `target/isolated/`, but `build.rs:44-55` copies `LLVM-C.dll` next to the cargo profile binaries (`target/<profile>/`), so the isolated executables resolve the DLL only via PATH (inference from code; not run).
- `src/pkg/mod.rs:100` `Constraint::matches`/`Version` have no pre-release representation; `docs/REGISTRY.md` acknowledges this.

---

## Scope and coverage

- **Read in full:** `src/cli.rs`, `src/modules/mod.rs`, `src/pkg/mod.rs`, `src/fmt.rs`, `src/main.rs`, `src/lib.rs`, `build.rs`, `difftest.sh`, `tools/gate.sh`, `tools/native_diff.sh`, `.gitignore`; plus `src/repl/mod.rs` and `src/diagnostics/mod.rs` to verify REPL/stdin and exit-code mapping, and targeted reads of `src/interpreter/mod.rs`, `src/vm/mod.rs`.
- **Verified against intent:** CLI exit contract in the brief vs `run()` mapping (`src/cli.rs:110-122`) and every `Phase::Usage` site; module doc comment vs `resolve`/cache; `docs/REGISTRY.md` layout/constraints/`sha256.txt` vs `pkg` implementation; `docs/BUILDING.md` vs `build.rs` feature gating.
- **Checked and found sound:** SHA-256 padding/length/compression (`src/pkg/mod.rs:634-700`) matches FIPS 180-4 and the in-repo known-answer tests (`:1146`); exit mapping 0/1/2 in `run()`; cycle detection terminates (bounded by distinct import spellings); `module_interp` receives `_importer_globals` unused, so modules do **not** leak private top-level bindings into the importer's globals; `gate.sh`/`native_diff.sh` quoting and timeout handling.
- **Not verified (no build/run permitted):** all runtime behavior above is traced from code, not observed; the `fmt` `=-1` result is a deterministic code trace; git option-injection impact depends on the installed git version's argument parsing; `LLVM-C.dll` search-order note is inferred from `build.rs` and Windows loader order; no fuzzing of `read_tar`/`parse_manifest` beyond manual edge-case reading.
