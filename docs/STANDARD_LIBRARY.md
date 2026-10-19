# Spid3r Standard Library

Global functions available in every program. These are ordinary global values
(not keywords), so a user declaration of the same name shadows the builtin.

## Console & output

| Function | Description |
|---|---|
| `print(x)` | Print `x` with no trailing newline. |
| `println(x)` | Print `x` followed by a newline. |
| `eprint(x)` | Print `x` to standard error. |

## Input

| Function | Description |
|---|---|
| `read_line()` | Read one line from standard input (`null` at EOF). |

## Files & environment

| Function | Description |
|---|---|
| `read_file(path)` | Read a text file; returns `ok(text)` / `err(msg)`. |
| `write_file(path, text)` | Write a text file; returns `ok(null)` / `err(msg)`. |
| `file_exists(path)` | `true` if the path exists (files and directories). |
| `env(name)` | Read an environment variable as an `option` (`some(value)` / `none`). |
| `args()` | The arguments passed to the program: everything after the script path on the `spid3r run` command line. |

## Time & system

| Function | Description |
|---|---|
| `sleep_ms(n)` | Sleep for `n` milliseconds (yields the task). |
| `now()` | Millisecond timestamp, monotonic from the process's first `now()` call. |
| `spawn_process(cmd, args)` | Run an external command to completion (synchronous, argv passed directly, no shell); returns `ok(exit_code)` / `err(msg)`. |
| `yield()` | Cooperatively yield the current task to the scheduler. |
| `task_count()` | Number of live scheduler tasks. |

## Math & numbers

| Function | Description |
|---|---|
| `abs(x)` | Absolute value. |
| `sqrt(x)` | Square root. |
| `pow(base, exp)` | Exponentiation. |
| `min(a, b)` / `max(a, b)` | Minimum / maximum. |
| `random_int(lo, hi)` | Random integer in `[lo, hi]` (seeded per thread from real entropy; differs across runs). |

## Conversions

| Function | Description |
|---|---|
| `int(x)` | Convert to integer. |
| `float(x)` | Convert to float. |
| `string(x)` | Convert to a string representation. |
| `bool(x)` | Convert to boolean (truthiness). |

## JSON

| Function | Description |
|---|---|
| `json_parse(text)` | Parse JSON into Spid3r values (including `\uXXXX` escapes and surrogate pairs); nesting is capped at 512 levels. |
| `json_write(value)` | Serialize a value to a JSON string. Non-finite floats are an error; nesting is capped at 512 levels (self-referential containers get a clean error, not a hang). |

## Networking & HTTP

Built directly on `std::net` with zero third-party dependencies. Sockets are represented as integer handles.

| Function | Description |
|---|---|
| `tcp_listen(addr)` | Bind a TCP listener (`ok(listener_id)` / `err(msg)`). Use `"127.0.0.1:0"` to pick a free port. |
| `tcp_accept(listener_id)` | Accept an incoming connection (`ok(conn_id)` / `err(msg)`). Blocks until a client connects. |
| `tcp_connect(addr)` | Connect to a remote TCP host (`ok(conn_id)` / `err(msg)`). |
| `tcp_read(conn_id, max_bytes)` | Read up to `max_bytes` (`ok(text)`). Returns `ok("")` on peer close. |
| `tcp_write(conn_id, text)` | Write UTF-8 string data (`ok(bytes_written)` / `err(msg)`). |
| `tcp_close(handle)` | Close a connection or listener; returns `true` if handle was open. |
| `tcp_local_addr(handle)` | Bound local address string (`ok("127.0.0.1:port")`). |
| `tcp_peer_addr(conn_id)` | Peer remote address string (`ok("ip:port")`). |
| `tcp_set_timeout(conn_id, ms)` | Set socket read/write timeout in milliseconds (`0` disables). |
| `http_read_request(conn_id)` | Read and parse an HTTP 1.1 request (`ok(req)` map with method, path, query, headers, body). |
| `http_write_response(conn_id, status, headers, body)` | Send an HTTP 1.1 response (`ok(bytes_sent)`). |
| `http_fetch(method, url, headers?, body?)` | High-level HTTP client request (`ok(res)` map with status, headers, body). |
| `url_decode(s)` | Decode percent-encoded string (with form `+` as space). |
| `url_encode(s)` | Percent-encode string adhering to RFC 3986. |

## Introspection

| Function | Description |
|---|---|
| `len(x)` | Length of a collection or string. |
| `type_of(x)` | The type name of a value (`int`, `list`, …). |

## Notes

- `print`/`println` use each value's `display` form; collections render like
  `list[1, 2, 3]`, `set{1, 2}`, `map{k: v}`, and user classes as their field map.
  (`string(list)` prints the bare `[1, 2]` form.)
- File and process operations that can fail return a `result` (`ok`/`err`)
  rather than throwing, so callers can handle failures with `is_ok`/`is_err` and
  `unwrap`. Argument/type errors still throw.
