# Environment variables

Every knob Spid3r reads from the environment, in one place. Grouped by when they
matter; "default" is what happens when the variable is unset. A flag variable is
**on** only when set to something other than `0`, `false`, `off` or `no` (an
empty value counts as on for the build flags, off for `SPID3R_GC`).

## Runtime

| Variable | Default | Effect |
|---|---|---|
| `SPID3R_PATH` | — | Extra module search directories, `;`-separated. Searched after the importing file's directory and before `./stdlib`. |
| `SPID3R_MAX_WORKERS` | `256` | Ceiling for the grow-on-demand task pool. At the ceiling a stall is reported on stderr instead of hanging. |
| `SPID3R_QUEUE_CAP` | `65536` | Bound on the shared ready queue. A producer that fills it parks until a worker drains one job. |
| `SPID3R_DEADLOCK_ABORT` | on | When no task can ever wake a wait, abort with a diagnostic (Go's "all goroutines are asleep"). `0` downgrades it to a warning for hosts that supervise their own waits. |
| `SPID3R_TASK_DRAIN_MS` | `2000` | How long program exit waits for still-running tasks before warning and exiting anyway. `0` disables the wait. |
| `SPID3R_GC` | on | Automatic cycle collection at statement boundaries. `0` disables it; `gc_collect()` still works. |
| `SPID3R_NET_TIMEOUT_MS` | `30000` | Deadline for HTTP framing (slowloris guard); `0` disables it. Raw `tcp_*` connections keep whatever timeout the program sets. |
| `SPID3R_SQLITE_PATH` | — | Directory searched for `sqlite3.dll` / `libsqlite3.so` before the default lookup. |
| `SPID3R_CACHE_DIR` | `%LOCALAPPDATA%\Spid3r` (Windows), `$XDG_CACHE_HOME/spid3r` (else) | Root for the extracted-runtime cache (`runtime/`). The log has its own path (`SPID3R_LOG_FILE`). |
| `SPID3R_EXE` | — | Runtime a packaged app launches **when it carries no embedded copy** (checked before the per-user install and `PATH`). An embedded runtime always wins. |
| `SPID3R_LOG` | on | `0` disables the diagnostic log entirely. |
| `SPID3R_LOG_FILE` | `%LOCALAPPDATA%\Spid3r\logs\spid3r.log` | Log path override. |
| `SPID3R_RUN_PAYLOAD_DEBUG` | off | The launcher prints the runtime it chose and the payload path to stderr. |

## Building and the native backend

| Variable | Default | Effect |
|---|---|---|
| `SPID3R_LLVM_LIB_DIR` | auto | Directory holding `LLVM-C.lib` (Windows) or the LLVM shared library (Unix). |
| `SPID3R_LLVM_BIN_DIR` | auto | Directory holding `LLVM-C.dll`; also prepended to `PATH` for the delay-load. |
| `SPID3R_LIB_DIR` | auto | Directory searched for the runtime staticlib (`spid3r_lang.lib` / `libspid3r.a`). |
| `SPID3R_RUNTIME_LIB` | auto | Explicit path to that staticlib (a file, or a directory containing it). |
| `SPID3R_DUMP_IR` | off | `spid3r build` prints the generated LLVM IR to stderr. |
| `SPID3R_TARGET` | — | Compile-time: the target triple the toolchain was built for, injected by `build.rs`. Read-only; not a user setting. |

## Packaging (Windows installer)

| Variable | Default | Effect |
|---|---|---|
| `SPID3R_NO_INSTALLER` | off | Skip the installer step for this build. |
| `SPID3R_PACKAGE` | off | Build the installer even when the profile is not `release`, and force the step to re-run. |
| `SPID3R_BUILD_INSTALLER`, `SPID3R_INSTALLER` | off | Aliases for `SPID3R_PACKAGE` (`0` explicitly disables). |
| `SPID3R_INSTALLER_NESTED` | — | Internal recursion guard set by `build.rs` for the nested release build. Do not set it. |
| `SPID3R_FETCH_WEBVIEW2` | off | Download Microsoft's WebView2 bootstrapper into the installer. Off by default: a plain build performs **no** network access. |
| `SPID3R_WEBVIEW2_SHA256` | pinned | Expected digest of that download; required when `SPID3R_FETCH_WEBVIEW2=1` is used with a rotated artifact. |

## Dependencies (`spider.toml` / `spid3r install`)

| Variable | Default | Effect |
|---|---|---|
| `SPID3R_REGISTRY` | — | Root directory of a local package registry. Without it, a `registry` dependency fails with a clear message. |
| `SPID3R_ALLOW_INSECURE_GIT` | off | Permit `http://` / `git://` git dependencies. Off because they carry no transport integrity; `https://` needs no flag. |
| `SPID3R_ALLOW_LOCAL_GIT_DEPS` | off | Permit bare paths and `file://` URLs as git dependencies. |

## Notes

- Flags that gate security-relevant behaviour (`SPID3R_ALLOW_INSECURE_GIT`,
  `SPID3R_ALLOW_LOCAL_GIT_DEPS`, `SPID3R_DEADLOCK_ABORT=0`,
  `SPID3R_FETCH_WEBVIEW2`) are opt-in for a reason; see `docs/SECURITY.md`.
- Nothing here is required for a normal `spid3r run` / `compile` / `build`.
