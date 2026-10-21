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
| `exec_capture(cmd, args, stdin_text)` | Run a command and capture its output. `stdin_text` is a string written to the child's stdin before waiting (`null` closes stdin immediately). Returns `ok(map("code", exit_code, "stdout", stdout_str, "stderr", stderr_str))`, or `err(msg)` if the process cannot be spawned or its output is not valid UTF-8. Each pipe is capped at 64 MiB. |
| `yield()` | Cooperatively yield the current task to the scheduler. |
| `task_count()` | Number of live scheduler tasks. |

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

## Crypto

| Function | Description |
|---|---|
| `sha256(s)` | SHA-256 (FIPS 180-4) of the UTF-8 bytes of `s`, lowercase hex — the same implementation the package layer uses to verify dependencies. |
| `hmac_sha256(key, msg)` | RFC 2104 HMAC-SHA256 of `msg` under `key`, lowercase hex, over the standard 64-byte block. |

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
| `unix_ms()` | Current Unix epoch timestamp in milliseconds (`int`). |
| `datetime_parts(epoch_sec)` | Decompose Unix timestamp into map (`year`, `month`, `day`, `hour`, `minute`, `second`, `weekday`). Supports negative and leap-year timestamps. |
| `datetime_from_parts(y, m, d, h, min, s)` | Convert civil date-time components into epoch seconds (`int`). |
| `format_datetime(epoch_sec, fmt_str)` | Strftime-style formatting (`%Y`, `%m`, `%d`, `%H`, `%M`, `%S`, `%w`, `%F`, `%T`, `%%`). |

---

# First-Party Standard Library Modules (`./stdlib/`)

Spid3r includes four production-grade, zero-dependency standard library modules located in `./stdlib/`. These modules are automatically searched by `modules::default_module_dirs()` and can be imported using flat (`import "name";`) or namespaced (`import "name" as alias;`) imports. Every module runs identically across both the tree-walk interpreter and the bytecode VM.

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

## Notes

- `print`/`println` use each value's `display` form; collections render like
  `list[1, 2, 3]`, `set{1, 2}`, `map{k: v}`, and user classes as their field map.
  (`string(list)` prints the bare `[1, 2]` form.)
- File and process operations that can fail return a `result` (`ok`/`err`)
  rather than throwing, so callers can handle failures with `is_ok`/`is_err` and
  `unwrap`. Argument/type errors still throw.


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
| `bst()` | ordered: `insert contains remove root` (+ `len is_empty clear clone`) |
| `binary_tree()` | `insert root` (+ `len is_empty clear clone`) |
| `heap()` | `push pop peek is_min` (+ `len is_empty clear clone`) |
| `priority_queue()` | `push pop peek` (+ `len is_empty clear clone`) |
| `ring_buffer(n)` | bounded: `push peek pop is_full capacity contains` (+ `len is_empty clear clone`) |
| `trie()` | prefix tree: `insert contains starts_with with_prefix all_words` (+ `len is_empty clear clone`) |
| `graph(directed?)` | `add_node has_node add_edge neighbors path_exists find_path bfs dfs nodes edge_count is_directed` (+ `len is_empty clear clone`) |
| `vector(…)` | numeric: `get set push slice to_list add sub mul div sum mean variance std min max argmin argmax dot norm cumsum diff sort argsort save_npy` (+ `len is_empty clear`) |
| `matrix(rows, cols, fill)` | 2-D numeric: `get set row col to_list transpose scale trace sum mean solve inverse det eigen_sym save_npy` (+ `rows cols is_empty`) |
| `refbox(v)` | single mutable cell: `get set` |

Two conventions to know: **stack/queue both use `push`/`pop`** — a stack pops
from the top, a queue pops from the front; and `remove` on `list` removes *by
value* while `remove_at` removes *by index*. Method names may collide with
user names freely (`get`, `len`, `keys` are ordinary identifiers, not
keywords); module namespaces have their own precedence rule — an exported
name wins over a built-in method of the namespace map.
