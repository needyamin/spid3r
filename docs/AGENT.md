# Runtime configuration and remote management

Spid3r ships a self-describing runtime: one `spid3r.ini`, one `spid3rinfo()`,
and one encrypted management agent. This page is the operator manual. The
implementation lives in `src/config/`, `src/runtime_info/`, `src/agent/` and
`src/crypto/`.

## 1. `spid3r.ini`

A single INI file configures the runtime. Search order (first hit wins):

1. `$SPID3R_INI`
2. `spid3r.ini` next to the `spid3r` executable
3. platform config dir (`%APPDATA%\Spid3r`, `$XDG_CONFIG_HOME/spid3r`, `~/.config/spid3r`)
4. `./spid3r.ini`

`spid3r config init` writes a commented default. `spid3r config path` prints
the search list. `spid3r config validate` reports unknown keys and type
errors; it never aborts a run.

Precedence, highest first:

1. process environment (`SPID3R_<SECTION>_<KEY>`, plus the documented
   `SPID3R_MAX_WORKERS`, `SPID3R_GC`, …)
2. `[env.<name>]` where `<name>` is `SPID3R_ENV` or `[spid3r] env`
3. the INI file
4. compiled defaults (`src/config/schema.rs`)

Live reload: `spid3r config reload`, `SIGHUP` on Unix,
`POST /api/v1/config/reload` on the agent, or mtime polling on the next CLI
run.

```ini
[spid3r]
engine = vm
max_workers = 256
gc = true

[memory]
max_steps = 0            ; 0 = unlimited
max_call_depth = 1000

[agent]
enabled = false
bind = 127.0.0.1         ; 0.0.0.0 to accept other hosts
port = 9527

[security]
allow_remote = false

[env.production]
agent.enabled = true
agent.bind = 0.0.0.0
```

Scripts read and write the live values with `config_get(key)`,
`config_set(key, value)` and `config_reload()`. Sensitive-looking values are
redacted on read unless the caller is local.

## 2. `spid3rinfo()`

Spid3r's own runtime introspection. One collector, three renderers, one
redaction pass.

| Call | Result |
|---|---|
| `spid3rinfo()` | map of `section -> {key: value}` |
| `spid3rinfo("html")` | standalone HTML page |
| `spid3rinfo("json")` | JSON string |
| `spid3rinfo("text")` | aligned plain text |
| `spid3rinfo("map", true)` | map without redaction (local operator choice) |

CLI:

```console
$ spid3r info            # text
$ spid3r info --json
$ spid3r info --html
$ spid3r info --no-redact
```

Sections: Spid3r (version, language, Rust, profile), System (OS, arch,
memory, CPUs), Installation (exe, ini, log, state), Features (gui, native,
tls, wasm, agent), Modules (embedded stdlib, plugins), Configuration,
Environment, Memory, Agent.

Keys matching `password`, `token`, `secret`, `shared`, `credential`,
`authorization`, `cookie`, `api_key`, `apikey`, `private` are replaced with
`[redacted sha256:<12 hex>]`. Remote callers never receive unredacted output.

## 3. Management agent

`spid3r agent start` listens on `[agent] bind:port` and speaks
`spid3r-agent/1` over TCP. There is **no unrestricted shell**. Every
operation is a predefined name checked against the caller's role.

```console
$ spid3r agent start
$ spid3r agent pair --role operator --name laptop-1
pairing code (operator): 9f2c…        # 48 hex chars, single use, short TTL
$ spid3r agent status
$ spid3r agent ops
```

On the administrator's machine:

```console
$ spid3r admin pair 192.168.1.10:9527 9f2c… --name laptop-1
$ spid3r admin --host laptop-1 ping
$ spid3r admin --host laptop-1 status
$ spid3r admin --host laptop-1 info
$ spid3r admin --host laptop-1 config.get --key agent.port
$ spid3r admin --host laptop-1 config.set --key agent.name --value web-1
$ spid3r admin --host laptop-1 logs.tail --lines 80
$ spid3r admin --host laptop-1 modules.list
$ spid3r admin --host laptop-1 devices.list
$ spid3r admin --host laptop-1 ops.run --op-name self_test
$ spid3r admin --host laptop-1 reload
```

Predefined operations (`ops.run --name …`):

`ping`, `reload_config`, `rotate_log`, `gc_collect`, `self_test`,
`dump_diagnostics`, `restart_runtime`.

`restart_runtime` spawns the same executable and exits. In production run
the agent under a service manager (Windows service, systemd unit) and treat
exit as "the manager restarts it".

### Roles

| Role | May call |
|---|---|
| `viewer` | `status`, `info`, `config.get`, `logs.tail`, `modules.list`, `devices.list` |
| `operator` | viewer + `config.set`, `ops.run`, `reload` |
| `admin` | everything, including `devices.revoke` and `pair.create` |

Lists are configurable via `agent.role_viewer` / `role_operator` /
`role_admin` (`*` = all).

### Pairing and encryption

Pairing is out-of-band: the host prints a 192-bit one-time code; the client
sends it once. Both sides derive the long-term shared key as
`SHA-256(code)`. The shared key **never crosses the wire**. The code is
single-use and expires (`agent.pairing_ttl_sec`, default 300s).

After pairing, every data frame is:

```
magic "SPID3R01" | version | kind | device[16] | nonce[12] | len u32 | payload
```

`kind = pair` is plaintext JSON (the code is the secret).
`kind = data` is ChaCha20 encrypted and HMAC-SHA256 authenticated
(encrypt-then-MAC). Subkeys come from HKDF-SHA256. The frame header is AAD,
so a swapped payload fails the tag. Nonces are random 96-bit values; the
server keeps a short replay window.

There is no TLS and no certificate store. The same protocol runs on
Windows, Linux and macOS. Bind `127.0.0.1` unless you actually need other
hosts; put a firewall in front of `0.0.0.0`.

### Audit

Every accepted or rejected operation is one JSON line in
`<state>/audit.log`:

```json
{"ts":1762252800,"device":"laptop-1","role":"operator","op":"config.set","ok":true,"peer":"192.168.1.4:51000","detail":""}
```

Disable with `[logging] audit = false`.

### State files

| Path | Contents |
|---|---|
| `<state>/devices.json` | paired devices (id, name, role, shared key) |
| `<state>/clients.json` | admin-client known hosts |
| `<state>/audit.log` | append-only audit |

`<state>` is `%LOCALAPPDATA%\Spid3r\state`, `$XDG_STATE_HOME/spid3r`, or
`SPID3R_STATE_DIR`. `devices.json` and `clients.json` are `0600` on Unix.

## 4. Security notes

* Scripts still run with process authority (see `docs/SECURITY.md`). The
  agent does not change that. It is a management plane, not a sandbox.
* No `exec`, no `spawn_process`, no path-based file write. `config.set`
  only accepts known, validated keys.
* Remote `info` and `config.get` are always redacted.
* A stolen `devices.json` is game over for that host: rotate by revoking
  the device and re-pairing. Keep the file off backups you do not encrypt.
* Pairing should happen on a network you trust. The code is high-entropy
  and short-lived, but it is not a PAKE.

## 5. Cross-platform

| Concern | Windows | Linux | macOS |
|---|---|---|---|
| Config dir | `%APPDATA%\Spid3r` | `$XDG_CONFIG_HOME/spid3r` | `~/.config/spid3r` |
| State dir | `%LOCALAPPDATA%\Spid3r\state` | `$XDG_STATE_HOME/spid3r` | `~/.local/state/spid3r` |
| Log | same tree as today | same | same |
| Reload | `config reload` / agent | `SIGHUP` + those | `SIGHUP` + those |
| CSPRNG | `RtlGenRandom` | `/dev/urandom` | `/dev/urandom` |
| Memory info | `GlobalMemoryStatusEx` | `/proc/meminfo` | `sysctl hw.memsize` |

No third-party crates were added. The agent, INI parser, ChaCha20, HMAC,
HKDF and Poly1305 (unused, kept with a vector) are in-tree.
