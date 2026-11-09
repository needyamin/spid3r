# Environment variables

Every knob Spid3r reads from the environment, in one place. Grouped by when they
matter; "default" is what happens when the variable is unset. A flag variable is
**on** only when set to something other than `0`, `false`, `off` or `no` (an
empty value counts as on for the build flags, off for `SPID3R_GC`).

## Runtime

| Variable | Default | Effect |
|---|---|---|
| `SPID3R_PATH` | — | Env alias of `modules.path` (canonical INI key): extra module search directories, `;`-separated. Searched after the importing file's directory and before `./stdlib`. |
| `SPID3R_PLUGINS` | — | Env alias of `modules.plugins` (canonical INI key): plugin search path. |
| `SPID3R_MAX_WORKERS` | `256` | Env alias of `spid3r.max_workers` (canonical INI key): ceiling for the grow-on-demand task pool. At the ceiling a stall is reported on stderr instead of hanging. |
| `SPID3R_QUEUE_CAP` | `65536` | Env alias of `spid3r.queue_cap` (canonical INI key): bound on the shared ready queue. A producer that fills it parks until a worker drains one job. |
| `SPID3R_DEADLOCK_ABORT` | on | When no task can ever wake a wait, abort with a diagnostic (Go's "all goroutines are asleep"). `0` downgrades it to a warning for hosts that supervise their own waits. |
| `SPID3R_TASK_DRAIN_MS` | `2000` | How long program exit waits for still-running tasks before warning and exiting anyway. `0` disables the wait. |
| `SPID3R_GC` | on | Env alias of `spid3r.gc` (canonical INI key): automatic cycle collection at statement boundaries. `0` disables it; `gc_collect()` still works. |
| `SPID3R_NET_TIMEOUT_MS` | `30000` | Env alias of `net.timeout_ms` (canonical INI key): deadline for HTTP framing (slowloris guard); `0` disables it. Raw `tcp_*` connections keep whatever timeout the program sets. |
| `SPID3R_SQLITE_PATH` | — | Env alias of `paths.sqlite_path` (canonical INI key): full path of the SQLite shared library tried before the default lookup. The library is loaded once per process, so a config reload cannot move it. |
| `SPID3R_CACHE_DIR` | `%LOCALAPPDATA%\Spid3r` (Windows), `$XDG_CACHE_HOME/spid3r` (else) | Env alias of `paths.cache_dir` (canonical INI key): root for the extracted-runtime cache (`runtime/`). The log has its own path (`SPID3R_LOG_FILE`). |
| `SPID3R_EXE` | — | Runtime a packaged app launches **when it carries no embedded copy** (checked before the per-user install and `PATH`). An embedded runtime always wins. |
| `SPID3R_LOG` | on | Env alias of `logging.enabled` (canonical INI key): `0` disables the diagnostic log entirely. |
| `SPID3R_LOG_FILE` | `%LOCALAPPDATA%\Spid3r\logs\spid3r.log` | Env alias of `logging.file` (canonical INI key): log path override. |
| `SPID3R_RUN_PAYLOAD_DEBUG` | off | The launcher prints the runtime it chose and the payload path to stderr. |

## Configuration, state, and the management agent

| Variable | Default | Effect |
|---|---|---|
| `SPID3R_INI` | — | **Authoritative** explicit path to `spid3r.ini`: the only ini candidate when set and non-empty (no fallback). See the dedicated section below. |
| `SPID3R_CONFIG_DIR` | platform config dir | Directory that may hold `spid3r.ini`. |
| `SPID3R_STATE_DIR` | `%LOCALAPPDATA%\Spid3r\state` / `$XDG_STATE_HOME/spid3r` | Env alias of `paths.state_dir` (canonical INI key): paired devices, admin-client book, audit log. |
| `SPID3R_ENV` | `default` (or `[spid3r] env`) | Env alias of `spid3r.env`: selects `[env.<name>]` overrides in the INI. Naming an overlay with no matching `[env.<name>]` section is a **silent no-op** — no warning is emitted and the plain values stay in effect. |
| `SPID3R_<SECTION>_<KEY>` | — | Per-key override (`SPID3R_AGENT_PORT`, `SPID3R_AGENT_BIND`, …). |
| `SPID3R_RUNTIME_WALL_CLOCK_LIMIT_MS` | `0` | Alias of `runtime.wall_clock_limit_ms`: fatal wall-clock deadline for `spid3r run`, in ms; `0` = unlimited. |
| `SPID3R_RUNTIME_MEMORY_LIMIT_MB` | `0` | Alias of `runtime.memory_limit_mb`: catchable soft cap on script-value heap, in MiB; `0` = unlimited. |
| `SPID3R_RUNTIME_TIMEZONE` | `UTC` | Alias of `runtime.timezone`: default zone for the datetime natives (`UTC`, `+06:00`, or an IANA name). |
| `SPID3R_ERRORS_LEVEL` | `info` | Alias of `errors.level`: minimum surfaced diagnostic level (`error`/`warn`/`info`). |
| `SPID3R_ERRORS_DISPLAY` | `false` | Alias of `errors.display`: mirror diagnostic log lines to stderr. |
| `SPID3R_ERRORS_LOG_FILE` | — | Alias of `errors.log_file`: dedicated error-log path (empty = shared log only). |
| `SPID3R_NET_MAX_BODY` | `8388608` | Alias of `net.max_body`: largest accepted HTTP request body, in bytes (413 above it). |
| `SPID3R_NET_MAX_HEADERS` | `64` | Alias of `net.max_headers`: most request headers accepted per request (431 above it). |
| `SPID3R_NET_KEEPALIVE_MS` | `5000` | Alias of `net.keepalive_ms`: idle window between keep-alive requests; `0` closes after each response. |
| `SPID3R_PATHS_CACHE_DIR` | — | Generic spelling of `paths.cache_dir`. Beats the INI; the short `SPID3R_CACHE_DIR` alias wins when both are set. |
| `SPID3R_PATHS_STATE_DIR` | — | Generic spelling of `paths.state_dir`. Beats the INI; the short `SPID3R_STATE_DIR` alias wins when both are set. |
| `SPID3R_PATHS_SQLITE_PATH` | — | Generic spelling of `paths.sqlite_path`. Beats the INI; the short `SPID3R_SQLITE_PATH` alias wins when both are set. |

See `docs/AGENT.md` for the full management-agent manual.

### `SPID3R_INI` is authoritative

`SPID3R_INI` is not merely the first candidate in a search order. When it is
set and **non-empty** it is the **only** ini candidate — there is no fallback:

* If the pinned file is missing or unreadable, the loader uses **compiled
  defaults only**. It never falls through to the executable-directory,
  platform-config, `SPID3R_CONFIG_DIR`, or working-directory `spid3r.ini`.
* The loader keeps a typed pin failure separate from value-validation notes;
  local `config validate`, `control status` and `spid3rinfo()` report it.
* This is **fail-closed** for program execution: `run`, `check`, `compile`,
  `exec`, `test`, `bench`, `debug`, `gui`, `repl` and `magic` refuse to start
  and exit 1 while the pin is broken.
* Diagnostic commands remain usable: `--version`, `help`, `info`,
  `config path` and `config validate`. `config validate` reports the broken
  pin as a problem (exit 1), and an unknown flag exits 2 (usage). Remote
  `config.set` refuses to persist while the pin is broken; a local
  `control config set` or `spid3r config init` is how an operator repairs it.
  Restart any long-lived process that initialized policy while the pin was
  broken; its fail-closed policy is intentionally sticky for that process.

An empty `SPID3R_INI` is treated as unset, so the normal search order applies.
When it is unset (or empty), the machine/cwd ini candidates are tried as
before. Local validation diagnostics keep policy values (`security.basedir`,
`paths.*`, `cache.bytecode_dir`) **raw** so an operator can see which entry is
wrong; only credential-like values are hashed there. Remote surfaces
(`info`, `config.get`, `dump_diagnostics` over the agent) never see those
values, the configuration-notes row, or the pin status at all.

## Building and the native backend

| Variable | Default | Effect |
|---|---|---|
| `SPID3R_LLVM_LIB_DIR` | auto | Directory holding `LLVM-C.lib` (Windows) or the LLVM shared library (Unix). |
| `SPID3R_LLVM_BIN_DIR` | auto | Directory holding `LLVM-C.dll`; also prepended to `PATH` for the delay-load. |
| `SPID3R_LIB_DIR` | auto | Directory searched for the runtime staticlib (`spid3r_lang.lib` / `libspid3r.a`). |
| `SPID3R_RUNTIME_LIB` | auto | Explicit path to that staticlib (a file, or a directory containing it). |
| `SPID3R_DUMP_IR` | off | `spid3r build` prints the generated LLVM IR to stderr. |
| `SPID3R_ICON` | icon beside `spid3r.exe` | Application icon used by `spid3r build` when `--icon` is not passed. The default resolves against the running executable, not the current directory. |
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
| `SPID3R_REGISTRY` | — | Env alias of `packages.registry` (canonical INI key): root directory of a local package registry. Without either spelling, a `registry` dependency fails with a clear message. |
| `SPID3R_ALLOW_INSECURE_GIT` | off | Env alias of `packages.allow_insecure_git`: permit `http://` / `git://` git dependencies. Off because they carry no transport integrity; `https://` needs no flag. |
| `SPID3R_ALLOW_LOCAL_GIT_DEPS` | off | Env alias of `packages.allow_local_git`: permit bare paths and `file://` URLs as git dependencies. |

The INI keys are canonical: the package manager reads them from the resolved
`spid3r.ini`, and these variables override them per layer exactly like every
other `SPID3R_*` alias (the generic `SPID3R_PACKAGES_REGISTRY`,
`SPID3R_PACKAGES_ALLOW_INSECURE_GIT`, `SPID3R_PACKAGES_ALLOW_LOCAL_GIT`
spellings also work; a generic spelling wins over the short alias when both
are set).

## Notes

- Flags that gate security-relevant behaviour (`SPID3R_ALLOW_INSECURE_GIT`,
  `SPID3R_ALLOW_LOCAL_GIT_DEPS`, `SPID3R_DEADLOCK_ABORT=0`,
  `SPID3R_FETCH_WEBVIEW2`) are opt-in for a reason; see `docs/SECURITY.md`.
- Nothing here is required for a normal `spid3r run` / `compile` / `build`.
- The path keys (`paths.cache_dir`, `paths.state_dir`, `paths.sqlite_path`,
  `cache.bytecode_dir`) are frozen: the management agent's remote `config.set`
  cannot relocate the host's write targets — edit `spid3r.ini` on the host
  instead.
