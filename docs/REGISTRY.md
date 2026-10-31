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
      sha256.txt           # optional: expected hash of this package's src/ tree
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
- **`sha256.txt`** (optional) is an integrity guard: if present, `install`
  compares it against the SHA-256 of the vendored `src/` tree and fails on
  mismatch. `spid3r pack` prints the hash you can paste here.

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

`spidr publish --to-dir <dir>` writes the layout above without any network:

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
$ spidr publish --to-dir ../registry
published to ../registry/mathlib/0.1.0
sha256: 308a5889…

# in a consumer project
[dependencies]
mathlib = { version = "^0.1.0" }

$ SPID3R_REGISTRY=../registry spidr install
$ spidr verify
ok: 1 vendored package(s) match spider.lock
```

`spidr search <query> --local` matches package names in `SPID3R_REGISTRY` and in
`vendor/`. It does not reach the network — there is no remote index yet.

## Integrity

`install` verifies a registry package's `sha256.txt` when the package ships one,
and `spidr verify` re-hashes every vendored tree against the `sha256` in
`spider.lock` afterwards. Editing one vendored file therefore fails the build
gate instead of being silently compiled:

```console
$ spidr verify
error[verify]: mathlib: vendor/mathlib does not match spider.lock
  expected 308a5889…
  actual   31e79255…
1 problem(s) across 1 checked package(s)
$ echo $?
1
```

It also reports a directory under `vendor/` that no lock entry covers — a
package nobody declared is not something to trust.

## What it does not do (v1)

- **No network fetch.** Point `SPID3R_REGISTRY` at a local/checked-out tree. A
  remote index + download is a deliberate future step (it needs an HTTP client,
  which the zero-dependency default avoids). `spidr publish --to-dir` writes the
  registry layout and `spidr search --local` reads it, so the whole cycle works
  offline; only *distribution* is missing.
- **No signature verification.** TLS plus the SHA-256 in `spider.lock` is the v1
  trust model. Ed25519 signing needs a new dependency and key management, and is
  out of scope; do not describe the checksum as a signature.
- **No version-range intersection across the graph.** A constraint is checked
  against the resolved version, not unified across importers: a diamond where
  two packages demand incompatible ranges is reported as a conflict, not solved
  by backtracking. Greedy highest-satisfying is the v1 rule.
- **Transitive resolution is supported** for all three source kinds (path, git,
  registry): `install` walks each dependency's own `[dependencies]` breadth-first
  with cycle detection and a same-name/different-source conflict check, so a
  registry package's dependencies are vendored too. (This section claimed the
  opposite until 2026-10-21; the code has done it since the workspace support
  landed.) There is still no *version-range intersection* across the graph — a
  constraint is checked against the resolved version, not unified.
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
files = 1
sha256 = "…"
```
