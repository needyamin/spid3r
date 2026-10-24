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
| `file_delete(path)` | Delete a file; returns `ok(null)` / `err(msg)`. |
| `dir_create(path)` | Create a directory (including parent directories); returns `ok(null)` / `err(msg)`. |
| `dir_remove(path)` | Remove a directory; returns `ok(null)` / `err(msg)`. |
| `env(name)` | Read an environment variable as an `option` (`some(value)` / `none`). |
| `args()` | The arguments passed to the program: everything after the script path on the `spid3r run` command line. |

## Time & system

| Function | Description |
|---|---|
| `sleep_ms(n)` | Sleep for `n` milliseconds (yields the task). |
| `now()` | Millisecond timestamp, monotonic from the process's first `now()` call. |
| `spawn_process(cmd, args)` | Run an external command to completion (synchronous, argv passed directly, no shell); returns `ok(exit_code)` / `err(msg)`. |
| `exec_capture(cmd, args, stdin_text)` | Run a command and capture its output. `stdin_text` is a string written to the child's stdin before waiting (`null` closes stdin immediately). Returns `ok(map("code", exit_code, "stdout", stdout_str, "stderr", stderr_str))`, or `err(msg)` if the process cannot be spawned or its output is not valid UTF-8. Each pipe is capped at 64 MiB. |
| `yield()` | Cooperatively yield the current task to the scheduler. |
| `task_count()` | Number of live scheduler tasks. |
| `task.cancel()` (task method) | Request cancellation: the task stops at its next cancellation point (instruction boundary in the VM, statement boundary in the tree engine, loop poll every 2048 iterations in the native backend, or a channel wait) and its result becomes the error "task cancelled". Returns `true` when the request was accepted (the task was still running), `false` when it had already finished. `task.status()` reports `"cancelled"`. |

## Math & numbers

Every math builtin accepts an `int` or a `float` and answers with a `float`
(except the two boolean predicates). Values outside the domain of a function
produce the usual float result (`nan()`, `inf()`, or a negative argument to
`sqrt`/`ln`), never an error.

| Function | Description |
|---|---|
| `abs(x)` | Absolute value. |
| `sqrt(x)` | Square root. |
| `pow(base, exp)` | Exponentiation. |
| `clamp(x, lo, hi)` | `x` limited to the closed range `[lo, hi]`; throws when `lo > hi` or a bound is not a number. |
| `floor(x)` / `ceil(x)` / `trunc(x)` / `round(x)` | Round toward −∞, toward +∞, toward zero, half away from zero. |
| `sign(x)` | `-1.0`, `0.0` or `1.0` (preserving the sign of zero); `nan()` for `nan()`. |
| `exp(x)` / `ln(x)` / `log10(x)` / `log2(x)` | Exponentials and logarithms. |
| `sin(x)` / `cos(x)` / `tan(x)` | Trigonometric functions, radians. |
| `asin(x)` / `acos(x)` / `atan(x)` / `atan2(y, x)` | Inverse trigonometric functions, radians. `atan2` takes the conventional `(y, x)` order and answers with the angle of the vector `(y, x)`. |
| `hypot(x, y)` | `sqrt(x² + y²)` without intermediate overflow or underflow. |
| `is_nan(x)` / `is_inf(x)` | `true` for a NaN / an infinite argument. |
| `float_or(text, default)` | Parse `text` as a float, falling back to `default` when it is not a number. A numeric argument passes through unchanged. |
| `nan()` / `inf()` | The float constants NaN and +∞. |
| `min(a, b)` / `max(a, b)` | Minimum / maximum. |
| `random_int(lo, hi)` | Random integer in `[lo, hi]` (seeded per thread from real entropy; differs across runs). |
| `random_float()` | Uniform float in `[0.0, 1.0)`. |
| `random_normal()` | One sample from the standard normal distribution (Box–Muller). |

Float literals may use scientific notation: `1.5e3`, `1e-5`, `2E+4` are all
single float tokens. A literal too large for `f64` is a clean lexer error
rather than an infinity or a panic.

## Crypto & Passwords

| Function | Description |
|---|---|
| `sha256(s)` | SHA-256 (FIPS 180-4) of the UTF-8 bytes of `s`, lowercase hex — the same implementation the package layer uses to verify dependencies. |
| `hmac_sha256(key, msg)` | RFC 2104 HMAC-SHA256 of `msg` under `key`, lowercase hex, over the standard 64-byte block. |
| `pbkdf2_sha256(password, salt, iterations)` | PBKDF2-HMAC-SHA256 (RFC 8018), 32-byte key as lowercase hex. `salt` is a string or bytes; `iterations` clamped to 1..=10_000_000. The primitive for password storage: store `(salt, iterations, hash)`. |
| `bcrypt_hash(password, cost=10)` | OpenBSD bcrypt password hash string (`$2b$...`). Cost clamped to 4..=16. |
| `bcrypt_verify(password, hash)` | Constant-time verification of password against bcrypt hash (`bool`). |
| `argon2(password, salt, time_cost, mem_cost_kib, parallelism, variant, key_len)` | Argon2 password hashing (RFC 9106); `variant` is `"i"`, `"d"`, or `"id"`. Returns lowercase hex string. |
| `scrypt(password, salt, n, r, p, key_len)` | scrypt key derivation (RFC 7914). Returns lowercase hex string. |
| `secure_compare(a, b)` | Constant-time string / byte equality check to mitigate timing side-channels (`bool`). |

## Encodings & Codecs

| Function | Description |
|---|---|
| `base64_encode(data)` / `base64_decode(text)` | RFC 4648 standard alphabet (+/, padded). `data` is a string or bytes; `decode` returns `bytes`. |
| `base64_url_encode(data)` / `base64_url_decode(text)` | URL-safe alphabet (-_, unpadded) — the JWT form. |
| `hex_encode(data)` / `hex_decode(text)` | Lowercase hex (two digits per byte); `decode` returns `bytes` and rejects odd lengths / non-hex characters. |
| `base32_encode(data)` / `base32_decode(text)` | RFC 4648 standard alphabet (A-Z2-7, padded); `decode` returns `bytes`. |

## Conversions & Codepoints

| Function | Description |
|---|---|
| `int(x)` | Convert to integer. |
| `float(x)` | Convert to float. |
| `string(x)` | Convert to a string representation. |
| `bool(x)` | Convert to boolean (truthiness). |
| `ord(s)` | Unicode codepoint of the first character of string `s` (`int`). |
| `chr(n)` | String containing the Unicode character with codepoint `n`. |

## Serialization & Binary Formats

| Function | Description |
|---|---|
| `json_parse(text)` | Parse JSON into Spid3r values (including `\uXXXX` escapes and surrogate pairs); nesting is capped at 512 levels. |
| `json_write(value)` | Serialize a value to a JSON string. Non-finite floats are an error; nesting is capped at 512 levels. |
| `msgpack_encode(val)` / `msgpack_decode(bytes)` | MessagePack binary serialization. `encode` returns `bytes`; `decode` returns `ok(val)` or `err(msg)`. |
| `cbor_encode(val)` / `cbor_decode(bytes)` | RFC 8949 Concise Binary Object Representation. `encode` returns `bytes`; `decode` returns `ok(val)`. |
| `pb_encode(map)` / `pb_decode(bytes)` | Protocol Buffers wire-format encoding and decoding. `map` maps field numbers to values or lists. |

## Compression & Archives

| Function | Description |
|---|---|
| `crc32(data)` | Compute CRC-32 checksum (IEEE 802.3 standard) of string or `bytes` (`int`). |
| `adler32(data)` | Compute Adler-32 checksum (RFC 1950) of string or `bytes` (`int`). |
| `deflate(bytes)` / `inflate(bytes)` | Raw DEFLATE (RFC 1951) streaming compression / decompression (`ok(bytes)`). |
| `gzip_compress(bytes)` / `gunzip(bytes)` | GZIP format (RFC 1952) with CRC-32 integrity checking (`ok(bytes)`). |
| `zlib_compress(bytes)` / `unzlib(bytes)` | ZLIB format (RFC 1950) with Adler-32 checksum (`ok(bytes)`). |
| `zip_create(list_of_file_maps)` | Create in-memory `.zip` archive buffer (`bytes`). Each item is `map("name", "...", "data", bytes_or_str)`. |
| `zip_list(zip_bytes)` | List filenames inside zip archive: `ok(list_of_names)` or `err(msg)`. |
| `zip_read(zip_bytes)` | Extract files from archive into a map of `name -> bytes`: `ok(map)` or `err(msg)`. |

## Media & Document Formats

| Function | Description |
|---|---|
| `png_encode(width, height, channels, pixel_bytes)` | Encode raw uncompressed pixel data into standard PNG file bytes (supports 1, 3, or 4 channels). |
| `png_decode(png_bytes)` | Decode PNG bytes: returns `ok(map("width", w, "height", h, "channels", c, "data", bytes))` or `err(msg)`. |
| `wav_encode(sample_rate, channels, bits_per_sample, samples_bytes)` | Encode PCM audio into RIFF WAV file bytes (supports 8, 16, 24, 32-bit PCM). |
| `wav_decode(wav_bytes)` | Decode WAV bytes: returns `ok(map("sample_rate", sr, "channels", ch, "bits", b, "data", bytes))`. |
| `pdf_write(ops_list, width=595, height=842)` | Generate standard PDF 1.4 document `bytes` from PDF content stream operator strings. |
| `pdf_escape(text)` | Escape string for PDF string literals (escaping `\`, `(`, `)`). |

## Memory & Garbage Collection

| Function | Description |
|---|---|
| `gc_collect()` | Trigger an immediate cycle of mark-and-sweep garbage collection across all engines. |
| `gc_live_cells()` | Return the current count of active heap-allocated value cells (`int`). |
| `weak(ref)` | Create a non-owning weak pointer to a `ref(v)` interior-mutable cell. |
| `deref(weak)` | Dereference a weak pointer: returns target `Value` or `null` if the reference target has been collected. |

## Process & entropy

| Function | Description |
|---|---|
| `random_bytes(n)` | `ok(bytes)` from the OS CSPRNG (`/dev/urandom`, `RtlGenRandom`). **Use this for tokens.** `random_int` is xorshift seeded from the clock — fine for sampling, unsuitable for anything an attacker can guess at. |
| `shutdown_requested()` | True once SIGINT (or SIGTERM on Unix) has been seen, or `request_shutdown()` was called. Installs the handler on first call; a second signal aborts the process. |
| `request_shutdown()` | Set the flag (an admin endpoint, a supervisor, a test). |
| `reset_shutdown()` | Clear the flag. |

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
| `tcp_read_exact(conn_id, n)` | Block until `n` bytes arrive (or the peer closes); `ok(bytes)`. The primitive for length-prefixed framing (FastCGI records, binary protocols). |
| `tcp_write_bytes(conn_id, bytes)` | Write raw bytes; `ok(bytes_written)`. |
| `bytes_concat([b1, b2, ...])` | Join bytes/strings into one `bytes` value. `+` is not defined on bytes, and pushing ints one at a time is O(n) allocations. |
| `tcp_recv_to_file(conn_id, path)` | Stream the connection to a file until the peer closes; `ok(bytes_written)`. The bytes never pass through a Spid3r string, so a binary download is byte-exact (`tcp_read` is UTF-8 **lossy** and would replace `0x80`/`0xFF` with U+FFFD). A partial file is left on a read error, deliberately. |
| `tcp_send_file(conn_id, path)` | Stream a file to the connection; `ok(bytes_written)`. Leaves the handle open — the caller closes it (FIN is what an FTP data connection needs). |
| `tcp_poll([conn_ids], timeout_ms)` | Readiness wait: blocks until at least one listed connection is readable/writable or the timeout expires (`0` = do not wait); returns `ok(map("readable", [...], "writable", [...]))` with connection ids. Backed by the platform readiness call (`WSAPoll` on Windows, `poll(2)` on Unix) — the OS wakes the caller the moment a socket is ready, and nothing is read ahead. A probe-loop fallback covers a failed platform call. |
| `udp_bind(addr)` | Bind a UDP socket (`port 0` = ephemeral); returns `ok(handle)`. |
| `udp_local_addr(handle)` | Local address string (`ok("ip:port")`). |
| `udp_send_to(handle, addr, text)` | Send one datagram (UTF-8 bytes of `text`); `ok(bytes_written)`. |
| `udp_recv_from(handle, max)` | Block for one datagram; `ok([text, "sender-ip:port"])`. Invalid UTF-8 → U+FFFD. |
| `udp_join_multicast(handle, group, iface)` / `udp_leave_multicast(...)` | Join/leave an IPv4 multicast group (`iface` `"0.0.0.0"` = default route). |
| `udp_set_multicast_ttl(handle, ttl)` | Outgoing multicast hop count (0..=255). |
| `udp_set_broadcast(handle, on)` | Enable/disable broadcast sends. |
| `ws_connect(url)` | WebSocket client (RFC 6455): `ws://` handshake with accept-key verification; returns `ok(handle)`. (`wss://` needs the `tls` feature build — not available in the zero-dependency default.) |
| `ws_accept(conn_id)` | Server side: upgrade an accepted TCP connection; returns `ok(handle)`. |
| `ws_send(handle, text)` | Send one masked text frame; `ok(bytes_written)`. |
| `ws_recv(handle)` | `ok(text)` (binary frames arrive as bytes), `ok(null)` at close; pings are auto-answered, fragments reassembled. |
| `ws_close(handle)` | Send a close frame and shut the socket. |
| `udp_close(handle)` | Close the socket; `true` if it was open. |
| `http_read_request(conn_id)` | Read and parse an HTTP 1.1 request (`ok(req)` map with method, path, query, headers, body). |
| `http_write_response(conn_id, status, headers, body)` | Send an HTTP 1.1 response (`ok(bytes_sent)`). |
| `http_fetch(method, url, headers?, body?)` | High-level HTTP client request (`ok(res)` map with status, headers, body). `https://` requires the `tls` feature (`--features tls`; native-tls → OS certificate store); without it an `err` result explains the rebuild. |
| `url_decode(s)` | Decode percent-encoded string (with form `+` as space). |
| `url_encode(s)` | Percent-encode string adhering to RFC 3986. |

## Introspection

| Function | Description |
|---|---|
| `len(x)` | Length of a collection or string. |
| `type_of(x)` | The type name of a value (`int`, `list`, …). |

## Binary I/O & NumPy `.npy`

| Function | Description |
|---|---|
| `read_bytes(path)` | Read binary file into `ok(bytes)` / `err(msg)`. |
| `write_bytes(path, bytes)` | Write `bytes` buffer into binary file; returns `ok(null)` / `err(msg)`. |
| `save_npy(path, matrix_or_vector)` | Save a `matrix` (2D) or `vector` (1D) into NumPy `.npy` v1.0 little-endian format. |
| `load_npy(path)` | Load NumPy `.npy` file into `ok(vector)` (1D) or `ok(matrix)` (2D). |

## RFC-4180 CSV

| Function | Description |
|---|---|
| `parse_csv(text, opts?)` | Parse CSV string into `ok(map("headers", list, "rows", list_of_lists))` with automatic type inference. `opts` supports `"delimiter"` and `"has_header"`. |
| `read_csv(path, opts?)` | Read and parse CSV file at `path`. |
| `write_csv(path, headers, rows)` | Write RFC-4180 formatted CSV data to `path`. |

## Regular Expressions (Thompson NFA)

Zero-dependency linear-time regex matching. Supports `.`, `*`, `+`, `?`, `^`, `$`, character classes `[...]`/`[^...]`, `\d`, `\w`, `\s`, `\D`, `\W`, `\S`, alternation `|`, and grouping `()`.

| Function | Description |
|---|---|
| `regex_match(pattern, text)` | Test if `pattern` matches `text`; returns `bool`. Malformed pattern returns `err(msg)`. |
| `regex_find(pattern, text)` | Find first match; returns `some(matched_str)` or `none`. |
| `regex_find_all(pattern, text)` | Find all non-overlapping matches; returns `list` of matching strings. |
| `regex_replace(pattern, text, repl)` | Replace all occurrences of `pattern` with `repl`. |

## Date & Time (Howard Hinnant Civil Calendar)
Monotonic and epoch time conversion with high-precision civil calendar math.

| Function | Description |
|---|---|
| `unix_time()` | Current Unix epoch timestamp in whole seconds (`int`). |
| `now_ms()` | Monotonic millisecond clock (`int`) — for measuring elapsed time. |
| `unix_ms()` | Current Unix epoch timestamp in milliseconds (`int`). |
| `datetime_parts(epoch_sec)` | Decompose Unix timestamp into map (`year`, `month`, `day`, `hour`, `minute`, `second`, `weekday`). Supports negative and leap-year timestamps. |
| `datetime_from_parts(y, m, d, h, min, s)` | Convert civil date-time components into epoch seconds (`int`). |
| `format_datetime(epoch_sec, fmt_str)` | Strftime-style formatting, always in UTC. Specifiers: `%Y` `%m` `%d` `%e` `%H` `%I` `%M` `%S` `%p` `%a` `%A` `%b` `%B` `%j` `%w` `%u` `%z` (always `+0000`) `%Z` (always `UTC`) `%F` `%T` `%%`. |

## Time Zones (IANA tzdata 2026e)

Backed by `src/stdlib/tzdata.bin`, generated by `tools/gen_tzdata.py`: 343 canonical zones plus 255 links,
transitions authoritative from 1900-01-01 to 2100-12-31, every zone verified against CPython's `zoneinfo`.
The blob is parsed once on first use; a program that never touches a zone pays nothing.

| Function | Description |
|---|---|
| `tz_names()` | Sorted list of every zone and link name (598). |
| `tz_info(zone, ts)` | `ok({zone, offset, abbr, isdst})` / `err("unknown time zone ...")`. `offset` is seconds east of Greenwich. |
| `tz_parts(zone, ts)` | `ok({year, month, day, hour, minute, second, weekday, offset, abbr, isdst})` — civil time in that zone. |
| `tz_from_parts(zone, y, mo, d, h, mi, s)` | Wall clock -> epoch seconds. A repeated hour resolves to the **earlier** instant; a nonexistent time (DST gap) moves forward past the gap. |
| `tz_format(zone, ts, fmt)` | `format_datetime` in a zone: `%z` is that zone's offset and `%Z` its abbreviation. |
| `tz_version()` | The tzdata release, e.g. `"2026e"`. |

## Unicode Normalization & Case Folding (UCD 15.1)

Tables generated by `tools/gen_unicode_tables.py` from CPython's `unicodedata`; the algorithm is UAX #15 in
`src/stdlib/unicode.rs` (recursive decomposition, canonical ordering, composition with the blocking rule,
algorithmic Hangul). Surrogates are not representable in Spid3r strings, so they are absent from the tables.

| Function | Description |
|---|---|
| `unicode_normalize(text, form)` | `form` is `"NFC"`, `"NFD"`, `"NFKC"` or `"NFKD"` (case-insensitive). |
| `unicode_is_normalized(text, form)` | True when `text` is already in that form. |
| `unicode_casefold(text)` | Full case folding (UCD `CaseFolding.txt`, C+F): `"ß"` -> `"ss"`, `"İ"` -> `"i"` + U+0307. |
| `unicode_strip_marks(text)` | NFD with every combining mark removed — the search-folding primitive. |
| `unicode_combining_class(ch)` | Canonical combining class of one character (0 for starters). |
| `unicode_version()` | The UCD version the tables were built from, e.g. `"15.1.0"`. |

---

# First-Party Standard Library Modules (`./stdlib/`)

Spid3r includes 29 zero-dependency standard library modules located in `./stdlib/`. These modules are automatically searched by `modules::default_module_dirs()` and can be imported using flat (`import "name";`) or namespaced (`import "name" as alias;`) imports. Every module runs identically across both the tree-walk interpreter and the bytecode VM.

```matrix
import "web" as w;
import "dataframe" as df;
import "stats" as st;
import "ml" as ml;
```

---

## 1. `web.matrix` — Express / PHP Alternative

Built on core networking and crypto primitives (`tcp_listen`, `tcp_accept`, `http_read_request`, `http_write_response`, `url_decode`, `json_parse`, `json_write`, `hmac_sha256`, and `bytes`).

### WebApp Router

| Method | Signature | Description |
|---|---|---|
| `web_app()` | `() -> WebApp` | Create a new WebApp router instance. |
| `app.get(path, handler)` | `(string, fn(req) -> res) -> null` | Register a `GET` route. Supports `:param` wildcards (e.g., `/users/:id`) and `*` catch-all. |
| `app.post(path, handler)` | `(string, fn(req) -> res) -> null` | Register a `POST` route. |
| `app.put(path, handler)` | `(string, fn(req) -> res) -> null` | Register a `PUT` route. |
| `app.delete(path, handler)` | `(string, fn(req) -> res) -> null` | Register a `DELETE` route. |
| `app.use(middleware)` | `(fn(req) -> res?) -> null` | Register middleware. Returning a response halts further execution. |
| `app.handle(req)` | `(map) -> map` | Route a request dictionary and return a response dictionary. |
| `app.listen(addr)` | `(string) -> null` | Start an HTTP server listening on `addr` (e.g., `"127.0.0.1:8080"`). Dispatches incoming connections concurrently. |

### Helpers & Utilities

| Function | Signature | Description |
|---|---|---|
| `parse_query(qs)` | `(string) -> map` | Parse a URL query string (`name=Alice+Smith&role=admin`) into key-value pairs with automatic percent-decoding. |
| `parse_form(body)` | `(string) -> map` | Parse an `application/x-www-form-urlencoded` body string into a map. |
| `json_response(status, data)` | `(int, value) -> map` | Create an HTTP response map with `status`, JSON body, and `Content-Type: application/json`. |
| `html_response(status, html)` | `(int, string) -> map` | Create an HTTP response map with HTML body and `Content-Type: text/html; charset=utf-8`. |
| `text_response(status, text)` | `(int, string) -> map` | Create an HTTP response map with plain text body. |
| `redirect_response(url, status)` | `(string, int?) -> map` | Create a 302/301 redirect response with `Location` header. |
| `escape_html(s)` | `(string) -> string` | Escape HTML special characters (`&`, `<`, `>`, `"`, `'`). |
| `render_template(tpl, ctx)` | `(string, map) -> string` | Render a mustache-style `{{ key }}` HTML template with automatic HTML escaping. |
| `parse_cookies(req_or_hdr)` | `(map or string) -> map` | Parse HTTP `Cookie` header into key-value pairs. |
| `set_cookie(res, name, val, opts)` | `(map, string, val, map?) -> map` | Append `Set-Cookie` header with options (`path`, `max_age`, `same_site`, `http_only`, `secure`). |
| `jwt_sign(payload, secret)` | `(map, string) -> string` | Sign a JWT using HMAC-SHA256 and base64url encoding. |
| `jwt_verify(token, secret)` | `(string, string) -> result` | Cryptographically verify a JWT and return `ok(payload_map)` or `err(msg)`. |

---

## 2. `dataframe.matrix` — Pandas Alternative

A columnar table structure backed by native `vector` (for numerical columns) and `list` (for categorical/mixed columns).

| Function / Method | Signature | Description |
|---|---|---|
| `from_records(list_of_maps)` | `(list) -> DataFrame` | Construct a DataFrame from a list of row maps. |
| `from_csv(path)` | `(string) -> result` | Load CSV file into a DataFrame, auto-converting numeric columns to native `vector`. |
| `to_csv(df, path)` | `(DataFrame, string) -> result` | Export DataFrame to a CSV file. |
| `df.select(cols)` | `(list) -> DataFrame` | Return a new DataFrame with selected columns. |
| `df.filter(predicate_fn)` | `(fn(row) -> bool) -> DataFrame` | Filter rows matching predicate function. |
| `df.sort_by(col, desc)` | `(string, bool) -> DataFrame` | Sort DataFrame by column in ascending or descending order. |
| `df.group_by(key, agg_spec)` | `(string, map) -> DataFrame` | Group by key column and apply aggregation functions (`mean`, `sum`, `count`, `min`, `max`). |
| `df.inner_join(df2, on_col)` | `(DataFrame, string) -> DataFrame` | Perform inner join on common key column. |
| `df.drop_na(col?)` | `(string?) -> DataFrame` | Drop rows containing `null` in the specified column (or all columns if `null`). |
| `df.fill_na(col, val)` | `(string, value) -> DataFrame` | Replace `null` entries in a column with `val`. |
| `df.head(n)` | `(int) -> DataFrame` | Return top `n` rows. |
| `df.describe()` | `() -> DataFrame` | Generate summary statistics (`count`, `mean`, `std`, `min`, `max`) for numeric columns. |
| `df.print_table()` | `() -> null` | Pretty-print formatted ASCII table to stdout. |

---

## 3. `stats.matrix` — SciPy Stats Alternative

Built on native `vector` reductions and mathematical kernels.

| Function | Signature | Description |
|---|---|---|
| `mean(x)` | `(vector or list) -> float` | Arithmetic mean. |
| `median(x)` | `(vector or list) -> float` | Median value. |
| `mode(x)` | `(vector or list) -> value` | Most frequent element. |
| `variance(x, ddof?)` | `(vector or list, int?) -> float` | Sample (ddof=1) or population (ddof=0) variance. |
| `std(x, ddof?)` | `(vector or list, int?) -> float` | Standard deviation. |
| `percentile(x, p)` | `(vector or list, float) -> float` | $p$-th percentile with linear interpolation. |
| `quantile(x, q)` | `(vector or list, float) -> float` | Quantile for $q \in [0.0, 1.0]$. |
| `iqr(x)` | `(vector or list) -> float` | Interquartile range ($Q_3 - Q_1$). |
| `covariance(x, y)` | `(vec, vec) -> float` | Sample covariance between two variables. |
| `pearson(x, y)` | `(vec, vec) -> float` | Pearson correlation coefficient $r \in [-1, 1]$. |
| `spearman(x, y)` | `(vec, vec) -> float` | Spearman rank correlation with tie handling. |
| `zscore(x)` | `(vector or list) -> vector` | Standardize data to zero mean and unit variance. |
| `normal_pdf(x, mu?, sigma?)` | `(float, float?, float?) -> float` | Gaussian probability density function. |
| `normal_cdf(x, mu?, sigma?)` | `(float, float?, float?) -> float` | Gaussian cumulative distribution function ($< 1.5 \times 10^{-7}$ error). |
| `t_test_ind(a, b)` | `(vec, vec) -> map` | Two-sample independent t-test (returns `statistic`, `df`, `pvalue`). |
| `chi_square(obs, exp?)` | `(vec, vec?) -> map` | Chi-square goodness-of-fit test. |
| `train_test_split(X, y, ratio?, seed?)` | `(any, any, float?, int?) -> map` | Random Fisher-Yates train/test split. |

---

## 4. `ml.matrix` — Scikit-Learn Alternative

Machine learning models and evaluation metrics built on native `matrix` (`solve`, `transpose`, `mul`, `eigen_sym`) and `vector`.

| Model / Function | Signature | Description |
|---|---|---|
| `linear_regression_fit(X, y)` | `(matrix, vector) -> map` | Fit Ordinary Least Squares with normal equations and Ridge regularization fallback. |
| `linear_regression_predict(model, X)` | `(map, matrix) -> vector` | Predict linear continuous targets. |
| `logistic_regression_fit(X, y, lr?, epochs?)` | `(matrix, vector, float?, int?) -> map` | Train binary logistic regression using gradient descent. |
| `logistic_regression_predict(model, X)` | `(map, matrix) -> vector` | Predict binary class labels ($0.0$ or $1.0$). |
| `knn_predict(X_train, y_train, X_test, k?)` | `(matrix, vector, matrix, int?) -> vector` | $k$-Nearest Neighbors classifier using Euclidean distances. |
| `kmeans_fit(X, k?, max_iter?)` | `(matrix, int?, int?) -> map` | $k$-Means clustering with centroid convergence and inertia calculation. |
| `pca_fit_transform(X, n_components?)` | `(matrix, int?) -> map` | Principal Component Analysis via symmetric eigen-decomposition (`eigen_sym`). |

### Evaluation Metrics

| Metric | Signature | Description |
|---|---|---|
| `accuracy(y_true, y_pred)` | `(vec, vec) -> float` | Classification accuracy. |
| `confusion_matrix(y_true, y_pred)` | `(vec, vec) -> map` | Returns `tp`, `fp`, `tn`, `fn`. |
| `precision(y_true, y_pred)` | `(vec, vec) -> float` | Positive predictive value ($TP / (TP + FP)$). |
| `recall(y_true, y_pred)` | `(vec, vec) -> float` | Sensitivity ($TP / (TP + FN)$). |
| `f1_score(y_true, y_pred)` | `(vec, vec) -> float` | Harmonic mean of precision and recall. |
| `mse(y_true, y_pred)` | `(vec, vec) -> float` | Mean squared error. |
| `rmse(y_true, y_pred)` | `(vec, vec) -> float` | Root mean squared error. |
| `r2_score(y_true, y_pred)` | `(vec, vec) -> float` | Coefficient of determination $R^2 \in (-\infty, 1.0]$. |

---

## 5. `smtp.matrix` — Mail (RFC 5321 + RFC 5322)

An ESMTP client and a message builder. Transport is `tcp_*`; **there is no TLS stream native**, so this
client is cleartext-only and says so instead of downgrading silently.

| Function | Description |
|---|---|
| `smtp_connect(host, port, opts?)` | Connect, read the greeting, send `EHLO` (falling back to `HELO`). `ok(SmtpClient)` / `err(msg)`. opts: `timeout_ms` (30000), `helo` ("localhost"), `allow_plaintext_auth` (false), `tls` (false — **`true` always fails**, because STARTTLS cannot be negotiated). |
| `smtp_message(spec)` | Build `{headers, body, envelope_from, envelope_to}`. spec: `from`, `to`, `cc`, `bcc`, `reply_to`, `subject`, `text`, `html`, `attachments` (list of `{path}` / `{data}` / `{content}` + optional `name`, `content_type`), `headers`, `date`, `message_id`. Chooses `text/plain`, `multipart/alternative`, or `multipart/mixed`; encodes non-ASCII bodies as base64, subjects as RFC 2047 encoded words, non-ASCII filenames as RFC 2231; **rejects** header injection and malformed addresses rather than escaping them. Bcc appears in the envelope only. |
| `smtp_render(msg)` | The exact DATA payload (CRLF, no dot-stuffing — `send` does that). |
| `smtp_header(msg, name, value)` | Add or replace one header line. |
| `smtp_send(config, msg)` | Connect, optionally `AUTH PLAIN` (`user`/`password`), send, quit. `ok(receipt)` with `code`, `text`, `recipients`, `bytes`. |

`SmtpClient` methods: `hello(name)`, `capabilities()`, `supports(cap)`, `auth_plain(user, pass)`,
`auth_login(user, pass)`, `send(msg)`, `send_raw(from, rcpts, text)`, `noop()`, `reset()`, `verify(addr)`,
`quit()`, `close()`. Every method returns `ok`/`err` except the authentication ones, which throw with an
explanation when the channel is cleartext.

## 6. `ftp.matrix` — FTP (RFC 959 + EPSV/MLSD)

| Function | Description |
|---|---|
| `ftp_connect(host, port, opts?)` | Greeting, `FEAT`, login, `TYPE`. opts: `user` ("anonymous"), `password`, `timeout_ms`, `type` ("I"), `epsv` (true), `pasv_host`, `allow_plaintext_auth` (false). |
| `ftp_with(host, port, opts, fn(client) { ... })` | Connect, run the callback, quit (or close on error). |
| `retr(remote, local)` / `stor(local, remote)` | Binary transfers through `tcp_recv_to_file` / `tcp_send_file`; `ok(bytes)`. |
| `retr_text(remote)` / `stor_text(remote, text)` | Text transfers (UTF-8; use the binary pair for anything else). |
| `list(path?)` / `names(path?)` | `MLSD` when advertised, otherwise `LIST` (Unix and DOS shapes parsed); entries are `{name, type, size, mode, modified, raw}`. |
| `pwd()`, `cwd(p)`, `cdup()`, `mkd(p)`, `rmd(p)`, `dele(p)`, `rename(a, b)`, `size(p)`, `type(m)`, `noop()`, `quit()`, `close()` | Control-channel operations, each returning `ok(...)`/`err(msg)`. |

Non-anonymous logins require `allow_plaintext_auth: true`, and are refused outright when the server advertises
`AUTH TLS` (the upgrade cannot be taken).

## 7. `ssh.matrix` — SSH/SFTP via the system OpenSSH client

Not a protocol implementation on purpose: key exchange, host-key signatures, AEAD ciphers and userauth are a
project, and a half-finished SSH client is a security hole. This module builds a validated argv and runs
`ssh` / `sftp` with `exec_capture` — no shell, fixed option whitelist, `BatchMode=yes`, host keys verified by
default.

| Function | Description |
|---|---|
| `ssh_exec(host, command, opts?)` | Run a remote command; `ok({code, stdout, stderr})` (255 = transport/auth failure). |
| `sftp_get(host, remote, local, opts?)` / `sftp_put(host, local, remote, opts?)` | Batch-mode transfers. |
| `sftp_batch(host, commands, opts?)` | Run a list of sftp batch commands (or one multi-line string); stops at the first failure. |
| `sftp_mkdir` / `sftp_remove` / `sftp_list` | Directory operations. |
| `ssh_argv(host, command, opts?)` / `sftp_argv(host, opts?)` | The exact argv, public because argument construction *is* the security boundary (and is unit-tested). |
| `ssh_version()` | `ok("OpenSSH_9.6p1 ...")` or `err` when OpenSSH is absent. |

opts: `user`, `port` (22), `key`, `known_hosts`, `connect_timeout` (10), `strict_host_key`
(`"yes"` default / `"accept-new"` / `"no"`), `compression`, `verbose`. A host may not start with `-` or
contain `@`, and `ProxyCommand` cannot be expressed at all — unknown option keys are ignored, never forwarded.

---

## 8. `tz.matrix` — IANA time zones

| Function | Description |
|---|---|
| `tz_zones(prefix?)` | Every zone and link, optionally filtered by prefix (`"Asia/"`). |
| `tz_valid(zone)` | True when the zone or a link to it exists. |
| `tz_offset(zone, ts)` / `tz_abbr(zone, ts)` / `tz_is_dst(zone, ts)` | Offset in seconds, abbreviation, DST flag. |
| `tz_diff(a, b, ts)` | Offset difference between two zones at one instant. |
| `tz_parts_of(zone, ts)` | The civil-parts map (as `tz_parts`). |
| `tz_make(zone, y, mo, d, h, mi, s)` | Wall clock -> epoch seconds. |
| `tz_wall_in(from, to, y, mo, d, h, mi, s)` | Same instant, the other zone's civil parts. |
| `tz_iso(zone, ts)` | `"2025-10-21T00:40:00+02:00"`. |
| `tz_describe(zone, ts)` | `"Mon 2025-10-20 18:40:00 EDT -0400"`. |
| `tz_http_date(ts)` | RFC 7231 IMF-fixdate in GMT, for HTTP headers. |
| `tz_day_bounds(zone, ts)` | `{start, end, hours}` for the local day — 23, 24 or 25 hours. |
| `tz_add_days(zone, ts, n)` | Shift by whole days keeping the wall clock (DST-safe). |
| `tz_data_version()` | The tzdata release. |

## 9. `unicode.matrix` — normalization and comparison

| Function | Description |
|---|---|
| `unicode_nfc/nfd/nfkc/nfkd(text)` | The four normalization forms. |
| `unicode_is_nfc(text)` | Shorthand for `unicode_is_normalized(text, "NFC")`. |
| `unicode_equal(a, b)` | Canonical equivalence (`NFC(a) == NFC(b)`). |
| `unicode_caseless_equal(a, b)` | UAX #31 default caseless matching (fold, then compare). |
| `unicode_search_key(text)` | Case-folded, mark-stripped form for a forgiving search index. |
| `unicode_sort_key(text, level?)` | List of code points to sort on; `level` 1/2/3 (default 3). |
| `unicode_compare(a, b, level?)` | -1 / 0 / 1 at the given strength. |
| `unicode_compare_keys(ka, kb)` | Compare two precomputed sort keys. |
| `unicode_sort(strings, level?)` | New sorted list, stable at equal keys. |
| `unicode_cluster_len(text)` | Base characters plus mark runs (not UAX #29). |
| `unicode_truncate(text, n)` | Cut to `n` clusters without splitting a mark off its base. |
| `unicode_slug(text)` | ASCII slug: `"Crème Brûlée"` -> `"creme-brulee"`. |

**Not UCA/DUCET.** Within a strength, order is by code point, so `é` sorts after `z`; there is no per-locale
tailoring and no CLDR data (month/weekday names are English). That is stated, not approximated.

---

## 10. `multipart.matrix` — file uploads

| Function | Description |
|---|---|
| `multipart_parse(body_bytes, content_type)` | Native: `ok({boundary, parts})`; each part is `{name, filename, content_type, headers, data, size, text}`. |
| `multipart_build(boundary, parts)` | Native: encode parts back into a body (clients, tests). |
| `multipart_form(req)` | `ok({fields, files, parts, boundary})` from a request (reads `body_bytes`, not the lossy `body`). |
| `multipart_file(form, name)` / `multipart_field(form, name, dflt)` | Lookups. |
| `multipart_safe_name(filename)` | Basename only, control characters dropped, Windows-illegal characters replaced, `.`/`..` rejected. |
| `multipart_save(part, dir, opts?)` / `multipart_save_all(form, dir, opts?)` | Write to `dir`; a collision becomes `name-1.ext` unless `overwrite: true`. |

Requests now carry `body_bytes` alongside `body`: the string view is lossy UTF-8, so an uploaded PNG must be
read from the bytes.

## 11. `session.matrix` — server-side sessions

| Function | Description |
|---|---|
| `session_store(secret, opts?)` | `ok(SessionManager)`. opts: `cookie_name` ("sid"), `ttl` (86400), `idle` (1800), `dir`, `secure`, `same_site` ("Lax"). Secret must be ≥16 chars. |
| `store.middleware()` | `fn(req, next)`: loads or creates `req["session"]`, saves after the handler, sets the cookie for a new session. |
| `store.load(req)` / `store.start(req, data)` / `store.save(s)` | Explicit control. `start` always mints a new ID (fixation defence). |
| `store.destroy(req)` / `store.destroy_id(id)` | Revoke; `destroy` returns the clearing cookie. |
| `store.flash(s, key, v)` / `store.flash_take(s, key)` | One-shot data across a redirect. |
| `store.count()` / `store.prune()` | Live rows; drop expired ones. |

## 12. `middleware.matrix` — the middleware library

Every function returns `fn(req, next)`; register with `app.use_next(...)`. Order matters: recover first,
access-log last.

| Function | Description |
|---|---|
| `mw_recover(opts?)` | A throwing handler becomes a 500 (message logged, never sent). |
| `mw_request_id(opts?)` | `req["request_id"]` + `x-request-id` response header. |
| `mw_access_log(opts?)` | method, path, status, duration, bytes, remote. `skip` ignores prefixes like `/healthz`. |
| `mw_security_headers(opts?)` | nosniff, frame-options, referrer-policy, COOP, optional CSP/HSTS. |
| `mw_cors(opts?)` | Origin/methods/headers/credentials, and a real preflight answer (204). |
| `mw_gzip(opts?)` | gzip text-ish responses above `min_bytes` when the client accepts it; adds `Vary`. |
| `mw_etag(opts?)` | Weak ETag from the body hash; `If-None-Match` becomes a 304. |
| `mw_body_limit(max_bytes)` | 413 before the handler runs. |
| `mw_basic_auth(user, password)` | `secure_compare`, 401 with `WWW-Authenticate`. |
| `mw_rate_limit(opts?)` | Fixed-window per remote address; 429 with `Retry-After`. |
| `mw_chain([...])` | Compose several into one. |

## 13. `fastcgi.matrix` — the nginx/Apache front door

| Function | Description |
|---|---|
| `fastcgi_serve(addr, dispatch, opts?)` | Listen and serve FastCGI forever. |
| `fastcgi_serve_on(listener, dispatch, opts?)` | Same, on a listener you bound (port 0 in tests). |
| `fastcgi_connection(conn, dispatch, opts?)` | Handle one connection (keep-connection honoured). |

opts: `log` (true), `watch_signals` (true). The request map is the same one the HTTP server builds, so
`web_app().handle` plugs straight in.

## 14. `mock.matrix` — test doubles

| Function | Description |
|---|---|
| `stub(value)` / `stub_throws(msg)` | A callable that ignores its arguments (accepts 0–4) and returns `value`, or throws. |
| `mock0/mock1/mock2/mock3(impl)` | A recording double: returns `{call, log}`. Arity-specific because a closure sees only the parameters it declares. |
| `mock_list(impl)` | The variadic form: the callable takes the arguments as one list. |
| `mock_calls(m)` / `mock_count(m)` / `mock_args(m, i)` / `mock_last_args(m)` / `mock_result(m, i)` | The recording. |
| `mock_called_with(m, args)` / `mock_assert_called_with(m, args)` | Exact-argument matching. |
| `mock_assert_called_once(m)` / `mock_assert_not_called(m)` / `mock_assert_count(m, n)` | Call-count assertions. |
| `mock_reset(m)` | Clear the log, keep the implementation. |

## 15. `testkit.matrix` — fixtures and a test runner

| Function | Description |
|---|---|
| `test_suite(name)` | A suite: `case(name, fn(ctx))`, `skip(name, reason)`, `before_each(hook)`, `after_each(hook)`, `run()`, `done()`. |
| `tmpdir(tag)` / `with_tmpdir(tag, fn(dir))` | A unique temp directory (removed by the second, even on a throw). |
| `fixture_write(dir, name, text)` | Write a fixture file, return its path. |
| `temp_root()` | `TEMP`, `TMPDIR` or `TMP`. |

Each case gets a fresh `ctx` with `dir` (created before, deleted after), `name` and `suite`. A case that
throws is reported and the suite continues; `done()` returns the failure count, which `main` should return.

## 16. `assert.matrix` — assertion library

| Function | Description |
|---|---|
| `eq(actual, expected, label)` | Assert equality, reporting pretty diff on failure. |
| `ne(actual, unexpected, label)` | Assert inequality. |
| `is_true(cond, label)` | Assert condition is true. |
| `near(actual, expected, eps, label)` | Assert float closeness within epsilon. |
| `contains(haystack, needle, label)` | Assert string or list contains item. |
| `throws(f, label)` | Assert closure throws a catchable exception. |
| `does_not_throw(f, label)` | Assert closure executes without throwing. |
| `is_ok(result, label)` / `is_err(result, label)` | Assert Result variant. |
| `fail(label)` | Unconditionally fail assertion. |

## 17. `bigint.matrix` — arbitrary-precision integers

| Function | Description |
|---|---|
| `from_int(n)` / `from_string(s)` | Parse arbitrary-length integers. |
| `add(a, b)` / `sub(a, b)` / `mul(a, b)` / `divmod(a, b)` | Exact arithmetic operations. Supports operator overloads `+`, `-`, `*`, `/`. |
| `pow(a, exp)` | Modular or standard exponentiation. |
| `cmp(a, b)` | Three-way comparison (`-1`, `0`, `1`). |
| `to_string(a)` | Format BigInt as base-10 decimal string. |

## 18. `complex.matrix` — complex numbers

| Function | Description |
|---|---|
| `of(re, im=0)` | Construct complex number `re + im i`. Supports `+`, `-`, `*`, `/`. |
| `add(a, b)` / `sub(a, b)` / `mul(a, b)` / `div(a, b)` | Complex arithmetic. |
| `abs(z)` / `arg(z)` | Modulus and phase in radians. |
| `polar(z)` / `from_polar(r, theta)` | Polar representation conversion. |
| `conj(z)` | Complex conjugate. |
| `to_string(z)` | Formatted `"a+bi"` representation. |

## 19. `decimal.matrix` — exact fixed-point decimals

| Function | Description |
|---|---|
| `from_int(n)` / `from_string(s)` | Exact decimal representation `{units, scale}`. Supports `+`, `-`, `*`, `/`. |
| `add(a, b)` / `sub(a, b)` / `mul(a, b)` / `div(a, b, prec=6)` | Non-floating-point financial and ledger arithmetic. |
| `cmp(a, b)` | Comparison without precision loss. |
| `to_string(d)` | Decimal string representation. |

## 20. `rational.matrix` — exact fractions

| Function | Description |
|---|---|
| `of(num, den=1)` / `from_string(s)` | Construct reduced fraction `{num, den}` with positive denominator. Supports `+`, `-`, `*`, `/`. |
| `add(a, b)` / `sub(a, b)` / `mul(a, b)` / `div(a, b)` | Exact fraction arithmetic in lowest terms. |
| `to_string(q)` | `"num/den"` format. |
| `to_decimal(q, places=6)` | Rounded fixed-point decimal string. |

## 21. `mathx.matrix` — advanced numerical algorithms

| Function | Description |
|---|---|
| `anova_oneway(groups)` | One-way ANOVA returning `{f, df_between, df_within, p}`. |
| `mann_whitney(a, b)` | Mann-Whitney U test for independent samples. |
| `ks_test(a, b)` | Two-sample Kolmogorov-Smirnov goodness-of-fit test (`{d, p}`). |
| `convolve(a, b)` | Discrete linear convolution of numeric sequences. |
| `interp_linear(x, y, x0)` | 1-D piecewise linear interpolation. |
| `sparse_from_dense(matrix)` / `sparse_to_dense(s)` | Compressed sparse matrix conversions. |
| `sparse_mul_vector(s, vec)` | Fast matrix-vector sparse multiplication. |
| `random_exponential(rate)` / `random_poisson(lambda)` | Non-uniform random variate generators. |

## 22. `gen.matrix` — generators & lazy streams

| Function | Description |
|---|---|
| `gen(fn(emit))` | Create cooperative fiber generator stream. |
| `range_gen(start, stop?, step?)` | Lazy numeric sequence generator. |
| `next(g)` | Fetch next item from generator (`null` when exhausted). |
| `take(g, n)` | Take first `n` items into a list and stop generator. |
| `collect(g)` | Drain entire generator into a `list`. |
| `map(g, f)` / `filter(g, f)` | Lazy transformation and filtering pipelines. |
| `close(g)` | Terminate generator and reclaim background task. |

## 23. `toml.matrix` — TOML parser

| Function | Description |
|---|---|
| `toml_parse(text)` | Parse TOML 1.0 subset into nested Spid3r maps and lists. |
| `toml_get(map, dotted_key)` | Look up nested keys using dot notation (e.g. `"server.limits.max"`). |

## 24. `yaml.matrix` — YAML parser

| Function | Description |
|---|---|
| `yaml_parse(text)` | Parse YAML subset (mappings, sequences, nested structures, scalars). |

## 25. `xml.matrix` — XML parser & writer

| Function | Description |
|---|---|
| `xml_parse(text)` | Parse XML document into node tree `{name, attrs, children, text}`. |
| `xml_attr(node, name)` | Get attribute value from node. |
| `xml_text(node)` | Extract trimmed decoded character text. |
| `xml_find(node, tag_name)` | Find direct child elements matching `tag_name`. |
| `xml_write(node)` | Serialize node tree back into XML string. |

## 26. `uuid.matrix` — UUID v4

| Function | Description |
|---|---|
| `uuid_v4()` | Generate random RFC 9562 UUID v4 string with OS entropy. |

## 27. `semver.matrix` — SemVer 2.0.0

| Function | Description |
|---|---|
| `parse(v)` | Parse semantic version into map `{major, minor, patch, prerelease, build}`. |
| `compare(v1, v2)` | Three-way comparison following SemVer 2.0.0 precedence (-1, 0, 1). |
| `satisfies(v, range_str)` | Check if version satisfies range constraints (e.g. `">=1.0.0 <2.0.0"`). |

## 28. `event.matrix` — event loop

| Function | Description |
|---|---|
| `start()` | Initialize fresh single-threaded event loop. |
| `on_readable(loop, conn_id, callback)` | Register readiness callback on socket handle. |
| `remove(loop, conn_id)` | Unregister socket handle. |
| `run(loop, timeout_ms=250)` | Dispatch readable I/O events until stopped. |
| `stop(loop)` | Signal event loop to terminate. |

## Native Database Drivers (Zero-Dependency)

In addition to SQLite (`db_open`), Spid3r includes zero-dependency native TCP database protocol drivers:

### MySQL Protocol Client (`mysql_*`)
- `mysql_connect(addr, user, password, db) -> ok(handle)`
- `mysql_query(handle, sql) -> ok([row_map, ...])`
- `mysql_exec(handle, sql) -> ok(affected_rows)`
- `mysql_close(handle) -> bool`

### PostgreSQL Protocol Client (`pg_*`)
- `pg_connect(addr, user, password, db) -> ok(handle)`
- `pg_query(handle, sql) -> ok([row_map, ...])`
- `pg_exec(handle, sql) -> ok(null)`
- `pg_close(handle) -> bool`

### Redis Client (`redis_*`)
- `redis_connect(addr, password=null) -> ok(handle)`
- `redis_command(handle, ...args) -> ok(response)` (RESP2 protocol parser supporting Simple Strings, Errors, Integers, Bulk Strings, and Arrays)
- `redis_close(handle) -> bool`

## Coverage — `spid3r run --coverage`

`spid3r run --engine vm --coverage [--coverage-out lcov.info] prog.matrix` collects per-line hit counts in the
VM dispatch loop and prints a percentage table to **stderr** (stdout stays the program's own, so differential
tests are unaffected). `--coverage-out` writes standard LCOV. VM only: the tree engine has no instruction
stream to hook, and the flag says so instead of reporting a number for a different engine.

## Notes

- `print`/`println` use each value's `display` form; collections render like
  `list[1, 2, 3]`, `set{1, 2}`, `map{k: v}`, and user classes as their field map.
  (`string(list)` prints the bare `[1, 2]` form.)
- File and process operations that can fail return a `result` (`ok`/`err`)
  rather than throwing, so callers can handle failures with `is_ok`/`is_err` and
  `unwrap`. Argument/type errors still throw.

## SQLite database (`db_open`)

`db_open(path)` opens (or creates) a SQLite database by loading the real
SQLite C library at runtime — **zero crate dependencies**. The loader looks
for `sqlite3.dll` / `libsqlite3.so` / `libsqlite3.dylib` in this order: the
`SPID3R_SQLITE_PATH` environment variable, the running executable's
directory, then the OS default search paths. A failed load is a catchable
error that names every location tried.

Connections default to WAL journaling and `foreign_keys=ON`. All statements
are parameterized (`?` placeholders — injection-safe). Column types map
TEXT→string, INTEGER→int, REAL→float, BLOB→bytes, NULL→null; parameters
accept null/int/float/string/bytes.

| Method | Returns | Notes |
|---|---|---|
| `db.exec(sql)` | `{changes, last_id}` | Multi-statement DDL (`CREATE TABLE …`) |
| `db.run(sql, params?)` | `{changes, last_id}` | INSERT/UPDATE/DELETE |
| `db.query(sql, params?)` | list of row maps | SELECT; one map per row |
| `db.close()` | null | Use afterwards → `database is closed` error |

Not available on WebAssembly (no OS file system). On Windows, drop
`sqlite3.dll` next to the `spid3r` executable.

## WebApp framework extras

Built on `stdlib/web.matrix` (the `web_app()` router):

- **`app.static_files(dir, url_prefix)`** — serve files from `dir` under
  `url_prefix` with content-type detection (html/css/js/json/png/jpg/svg/
  ico/wasm/txt) and verbatim binary bodies. `..` traversal → 403; misses →
  404.
- **Cookies** — a handler's response map may carry a `cookies` map
  (`cookie(name, value, opts)` builds one; opts: `path`, `max_age`,
  `expires`, `http_only`, `secure`). Each entry becomes its own `Set-Cookie`
  header. `http_write_response` accepts a `bytes` body for binary responses.
- **Sessions** — `new SessionStore(secret, cookie_name?)`: stateless
  signed-cookie sessions. `store.start(data_map)` returns the Set-Cookie
  value (JWT, one day); `store.read(req)` returns the verified data map or
  `null` (absent/tampered/expired); `store.clear()` returns the logout
  cookie. The data is authenticated but not encrypted — the client can read
  it, never forge it.

## Container methods (the complete matrix)

Every container is constructed by its builtin (`list(…)`, `stack()`, `map(…)`,
…) and exposes the methods below. `len(x)` / `x.len()` work on all of them;
`clone()` is a deep copy everywhere it appears. Methods that mutate return
`null`; lookup misses throw (catchable) unless noted.

| Constructor | Methods |
|---|---|
| `list(…)`, `vec(…)` | `len is_empty clear capacity push pop peek get set insert remove_at remove index_of contains reverse sort slice join append clone sorted reversed` |
| `stack()` | LIFO: `push pop peek contains to_list` (+ `len is_empty clear clone`) |
| `queue()` | FIFO: `push` (back) `pop` (front) `peek contains to_list` (+ `len is_empty clear clone`) |
| `deque()` | front-only deque: `push_front pop_front peek_front contains to_list` (+ `len is_empty clear clone`) |
| `linked_list()`, `singly_linked_list()`, `doubly_linked_list()` | `push_back push_front pop_front pop_back peek_front peek_back get contains remove to_list` (+ `len is_empty clear clone`) |
| `map(…)` | insertion-ordered: `set get remove delete has items contains keys values` (+ `len is_empty clear clone`); `m[k] = v` is an alias for `set` |
| `hash_table(…)` | `set get remove contains bucket_count load_factor keys` (+ `len is_empty clear clone`) |
| `set(…)` | `remove contains union difference to_list` (+ `len is_empty clear clone`; `add` via the shared host) |
| `bag(…)` | multiset: `remove count_of contains` (+ `len is_empty clear clone`) |
| `bst()` | ordered: `insert contains remove root inorder preorder postorder` (+ `len is_empty clear clone`) |
| `binary_tree()` | `insert root inorder preorder postorder level_order` (+ `len is_empty clear clone`) |
| `heap()` | `push pop peek is_min` (+ `len is_empty clear clone`) |
| `priority_queue()` | `push pop peek` (+ `len is_empty clear clone`) |
| `ring_buffer(n)` | bounded: `push peek pop is_full capacity contains` (+ `len is_empty clear clone`) |
| `trie()` | prefix tree: `insert contains starts_with with_prefix all_words` (+ `len is_empty clear clone`) |
| `graph(directed?)` | `add_node has_node add_edge neighbors path_exists find_path bfs dfs nodes edge_count is_directed` (+ `len is_empty clear clone`) |
| `vector(…)` | numeric: `get set push slice to_list add sub mul div sum mean variance std min max argmin argmax dot norm cumsum diff sort argsort save_npy` (+ `len is_empty clear`) |
| `matrix(rows, cols, fill)` | 2-D numeric: `get set row col to_list transpose scale trace sum mean solve inverse det eigen_sym save_npy` (+ `rows cols is_empty`) |
| `ref(v)` | interior mutable cell: `get set` (weak references via `weak`/`deref`) |

Two conventions to know: **stack/queue both use `push`/`pop`** — a stack pops
from the top, a queue pops from the front; and `remove` on `list` removes *by
value* while `remove_at` removes *by index*. Method names may collide with
user names freely (`get`, `len`, `keys` are ordinary identifiers, not
keywords); module namespaces have their own precedence rule — an exported
name wins over a built-in method of the namespace map.
