# Spid3r OCI Container Engine (`R11`)

> **Status:** `R11.1`–`R11.9` implemented across `src/oci/`, `src/container/`, `src/native/sys.rs` (`DP-7`), and `spid3r container`. Linux kernel namespace/overlayfs/cgroup execution is gated behind `#[cfg(all(target_os = "linux", feature = "container"))]`.

## Quickstart

```sh
# Pull an OCI image (verifies manifest, config, and every layer SHA-256)
spid3r container pull http://127.0.0.1:5000/app/demo:v1

# Create & inspect a container bundle
spid3r container create --name web http://127.0.0.1:5000/app/demo:v1
spid3r container ps
spid3r container inspect web

# Run on Linux (built with --features container)
spid3r container run --name web http://127.0.0.1:5000/app/demo:v1 /bin/echo ok

# Stop, remove, and garbage-collect unreferenced layers
spid3r container stop web
spid3r container rm web
spid3r container gc
```

## Architecture

- **`src/oci/`** — Cross-platform OCI Image Spec v1 parser (`manifest.rs`), Registry v2 HTTP(S) client with anonymous Bearer token flow and multi-arch host selection (`client.rs`), and hardened gzip/ustar layer pipeline with `.wh.` whiteouts, path-traversal refusal, and atomic cache commits (`layers.rs`).
- **`src/container/`** — Snapshotter (`snapshot.rs`: Linux `overlayfs` + copy-up fallback + refcounted GC), runtime state machine & OCI `config.json` bundle loader (`runtime.rs`), `spid3r0` bridge IPAM pool (`net.rs`), and daemon state recovery (`daemon.rs`).
- **`src/native/sys.rs` (`DP-7`)** — Sanctioned `unsafe` home for Linux syscall shims (`unshare`, `setns`, `mount`, `umount2`, `pivot_root`, `chroot`, `sethostname`, `prctl(PR_SET_NO_NEW_PRIVS)`, `fork`, `waitpid`, `kill`, `setuid`/`setgid`). Zero external crates; gated out of `--no-default-features` and `wasm32` builds.

## Security Model & Honest Limitations

- **Digest & signature verification:** Every config and layer blob is SHA-256 verified before caching or extraction; mismatches fail closed with exit code `4`. Optional Ed25519 manifest signature enforcement (`--require-sig --pubkey <hex>`) reuses `crate::pkg::ed25519`.
- **Tar hardening:** Absolute paths, drive prefixes, and `..` components are refused before writing any file. Uncompressed layer buffering is capped at 64 MiB (`MAX_LAYER_TAR_BYTES`).
- **Platform scope:** Image pull, verification, whiteout assembly, IPAM, OCI bundle creation, and CLI management (`pull`/`create`/`ps`/`inspect`/`logs`/`rm`/`gc`) run on Windows, macOS, and Linux. Kernel namespace/cgroup/overlayfs execution (`spid3r container start`/`run`) requires Linux built with `--features container`.
