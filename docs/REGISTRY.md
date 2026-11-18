# Dependency registry

A **registry dependency** lets a project pull a package by *name + version
constraint* instead of a path or git URL. It reuses the same vendoring and
`spider.lock` machinery as path/git dependencies — only the *resolution* step is
new.

```toml
# spider.toml
[dependencies]
strlib = { version = "^1.2" }              # from the default registry
internal = { version = "1.0", registry = "internal" }  # from a named sub-registry
```

`spid3r install` resolves each registry dependency, vendors its `src/` tree into
`vendor/<name>/`, and pins `source = "registry+<resolved-version>"` plus the
content SHA-256 in `spider.lock`. Re-running `install` is idempotent.

## The registry is a local directory (no network)

A registry is a directory named by the **`SPID3R_REGISTRY`** environment
variable. This keeps the default build zero-dependency and offline-safe: the
registry is just files on disk (a committed index, a shared drive, or a `file://`
tree you keep locally). There is no built-in HTTP client and no default remote
registry — if `SPID3R_REGISTRY` is unset, a registry dependency fails with a
clear message telling you to set it.

### Layout

```
$SPID3R_REGISTRY/
  mathlib/
    1.0.0/
      spider.toml          # [package] name/version/entry
      src/lib.matrix
      sha256.txt           # required: expected hash of this package's src/ tree
    1.2.0/
      spider.toml
      src/lib.matrix
      sha256.txt
  strlib/
    2.0.0/
      ...
```

- Each package is a directory `<name>/`, with one subdirectory per version.
- Version directory names must be `major.minor.patch` (or `major.minor`); the
  highest version satisfying the constraint is chosen.
- A package directory is exactly a normal Spid3r package: a `spider.toml` plus a
  `src/` tree (the same thing `spid3r pack` produces).
- **`sha256.txt`** (required for registry packages since 2026-11-06) is the
  integrity guard: `install` compares it against the SHA-256 of the vendored
  `src/` tree and **refuses to install** when the file is missing, unreadable,
  empty, or does not match. `spid3r pack`/`publish` always write it; a
  hand-built registry entry without one is rejected rather than vendored
  unverified. `spid3r pack` prints the hash you can paste here.

  The hash covers **every file** under `src/` (not just `*.matrix`) and is taken
  over the **raw bytes**: for each file in sorted order,
  `relative/path` `0x00` contents `0x00`, concatenated and hashed once. Paths use
  `/` separators on every platform. (Before 2026-10-21 the hash skipped
  non-`.matrix` files and decoded contents lossily as UTF-8, so a changed binary
  asset could hash the same — regenerate any `sha256.txt` you published with the
  old code by re-running `spid3r pack`.)

### Named sub-registries

`registry = "internal"` looks under `$SPID3R_REGISTRY/internal/<name>/…`,
letting one `SPID3R_REGISTRY` host several registries. Omit it for the default
(`$SPID3R_REGISTRY/<name>/…`).

## Version constraints

Same grammar as path/git dependencies (`src/pkg`):

| Form | Meaning |
| --- | --- |
| `^1.2` / `^1.2.3` | same major, at least the given minor/patch |
| `>=1.2.3` | at least this version |
| `=1.2.3` or bare `1.2.3` | exactly this version |

The chosen directory's `spider.toml` version must also satisfy the constraint
(checked after resolution, exactly like a path dependency).

## Publishing into a registry

`spid3r publish --to-dir <dir>` writes the layout above without any network:

```
<dir>/<name>/<version>/
  spider.toml
  src/…
  sha256.txt      # content hash of src/, checked on install
```

Republishing an existing `<name>/<version>` is **refused** rather than
overwritten: a registry entry is immutable, and silently replacing one would
make every `spider.lock` that pins it a lie. Bump the version instead.

The round trip is local and offline:

```console
$ spid3r publish --to-dir ../registry
published to ../registry/mathlib/0.1.0
sha256: 308a5889…

# in a consumer project
[dependencies]
mathlib = { version = "^0.1.0" }

$ SPID3R_REGISTRY=../registry spid3r install
$ spid3r verify
ok: 1 vendored package(s) match spider.lock
```

`spid3r search <query> --local` matches package names in `SPID3R_REGISTRY` and in
`vendor/`. It does not reach the network — there is no remote index yet.

## Integrity

`install` requires and verifies a registry package's `sha256.txt` (missing,
empty or mismatched checksums are a hard install error), and `spid3r verify`
re-hashes every vendored tree against the `sha256` in
`spider.lock` afterwards — a lock entry with no recorded hash also fails
`verify`. Editing one vendored file therefore fails the build
gate instead of being silently compiled:

```console
$ spid3r verify
error[verify]: mathlib: vendor/mathlib does not match spider.lock
  expected 308a5889…
  actual   31e79255…
1 problem(s) across 1 checked package(s)
$ echo $?
1
```

It also reports a directory under `vendor/` that no lock entry covers — a
package nobody declared is not something to trust.

## Remote registries, signing & dependency unification (R8.1–R8.4)

- **HTTP(S) remote registries (`R8.2`).** `SPID3R_REGISTRY` / `packages.registry`
  accepts both local directory paths and `http://` / `https://` registry base
  URLs. Remote registries serve `<base>/<name>/index.txt`,
  `<base>/<name>/<version>/package.tar`, and `<base>/<name>/<version>/sha256.txt`,
  and are cached under `.spid3r/registry/<url-hash>/<name>/<version>/` after
  verifying `sha256.txt` `tree` hashes and path-traversal safety.
- **Release `--no-default-features` vs `--features tls` tradeoff.** Portable
  Unix release tarballs are built with `--no-default-features --features native`
  (without `tls`) to keep release binaries compact, whereas Windows MSI builds
  enable `tls`. Builds without `tls` support `http://` registry mirrors and
  local directories cleanly and fail with a clear TLS feature diagnostic if
  given an `https://` registry URL.
- **Ed25519 package signing (`R8.3`).** `spid3r publish --to-dir` writes
  `<dir>/<name>/<version>/package.tar`, `<dir>/<name>/index.txt`,
  `<dir>/registry.key` (unless `SPID3R_SIGN_KEY` / `SPID3R_PACKAGES_SIGN_KEY`
  is supplied), `<dir>/registry.pub`, and `<dir>/<name>/<version>/sha256.txt.sig`
  (64-byte raw Ed25519 signature over the exact `sha256.txt` bytes). On
  `install` / `update`, when `packages.trust_pk` or `SPID3R_TRUST_PK` (64-hex
  Ed25519 public key) is configured, the client verifies `sha256.txt.sig` and
  refuses unsigned or tampered packages cleanly; when `trust_pk` is unset and a
  signature exists, it verifies against `<dir>/registry.pub` (or unsigned
  packages emit a warning on stderr before installing).
- **Version-range unification (`R8.4`).** Diamond dependencies across both
  single-project manifests and `[workspace]` members intersect all required
  semver constraints for each package name, re-selecting the highest registry
  version satisfying every constraint (for example `^1.0` + `=1.2.0` -> `1.2.0`)
  and reporting conflicting constraints cleanly instead of silently picking the
  first constraint visited.
- **No semver pre-releases / build metadata** — plain `major.minor.patch`.

## Example

```bash
export SPID3R_REGISTRY=/srv/spid3r-registry     # a directory laid out as above
spid3r install                                  # resolves { version = "^1.2" }
spid3r run src/main.matrix
```

`spider.lock` then records, e.g.:

```
[[package]]
name = "mathlib"
version = "1.2.0"
source = "registry+1.2.0"
used-by = "app"
files = 1
sha256 = "…"
```

`used-by` names the workspace member(s) that depend on the package (for the
single-project case it is simply the project).
