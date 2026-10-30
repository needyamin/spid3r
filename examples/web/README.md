# Spid3r Web & HTTP Server Examples

Spid3r provides high-performance, asynchronous, multi-threaded networking capabilities natively in the standard library. You can build fast REST APIs, microservices, and dynamic web applications using pure `.matrix` source code without any external runtime dependencies.

---

## How It Works

Spid3r provides two complementary modules for web server development:

```
┌────────────────────────────────────────────────────────┐
│                   Spid3r Web Stack                     │
│                                                        │
│  ┌──────────────────────────┐┌───────────────────────┐ │
│  │      import "http"       ││    import "web"       │ │
│  │  Low-level Multi-threaded││ High-level Web App    │ │
│  │   HttpServer Core Engine ││    Micro-framework    │ │
│  └─────────────┬────────────┘└───────────┬───────────┘ │
│                │                         │             │
│                ▼                         ▼             │
│   ┌─────────────────────────────────────────────────┐  │
│   │  Multi-threaded Worker Pool (Native Concurrency)│  │
│   │   - Parallel client request processing          │  │
│   │   - Thread-safe state via mutex() and ref()     │  │
│   │   - Zero external web servers needed            │  │
│   └─────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────┘
```

1. **`import "http"`**: Low-overhead, high-concurrency server engine (`HttpServer`). Features custom middleware chains (`app.before`), dynamic route parameters (`:name`), query-string decoding, and direct TCP worker dispatching.
2. **`import "web"`**: High-level application micro-framework (`w.web_app()`). Offers unified middleware pipelines (`app.use`), automated request deserialization, static HTML file serving, and high-level response constructors.
3. **Multi-Threaded Worker Pool**: Requests are handled concurrently across CPU cores. Shared state between requests is managed cleanly and safely using Spid3r's built-in concurrency primitives (`mutex`, `ref`, `channel`).

---

## Included Examples

### 1. Multi-Threaded Todo API & Web UI (`server.matrix`)
A complete REST service and dynamic web interface showcasing `import "http"`:
- **Thread-Safe State**: Protects shared in-memory data using `mutex(list())` and atomic IDs using `mutex(ref(1))`.
- **CORS Middleware**: Intercepts preflight `OPTIONS` requests automatically with `app.before()`.
- **Route Parameters**: Extracts URL variables seamlessly (e.g. `GET /api/hello/:name`).
- **Query Strings**: Parses URL query parameters (e.g. `GET /api/search?q=keyword`).
- **REST CRUD**: Exposes `GET /api/todos` and `POST /api/todos` with JSON validation.
- **Embedded Web UI**: Serves a modern HTML/CSS/JS frontend directly at `GET /`.

**Run Server (Tree Engine):**
```bash
spid3r run examples/web/server.matrix
```

**Run Server (Bytecode VM Engine):**
```bash
spid3r run --engine vm examples/web/server.matrix
```

Once running, navigate to **http://127.0.0.1:8080** or test endpoints via `curl`:
```bash
# Greet endpoint with route parameter
curl http://127.0.0.1:8080/api/hello/Alice

# Create a new todo item
curl -X POST -H "Content-Type: application/json" -d "{\"title\":\"Build production app\"}" http://127.0.0.1:8080/api/todos

# List all todo items
curl http://127.0.0.1:8080/api/todos

# Search query
curl "http://127.0.0.1:8080/api/search?q=spid3r"
```

---

### 2. CloudOps Management Hub (`cloudops_hub/`)
A production-grade web application server showcasing `import "web" as w` combined with Spid3r's data science libraries (`stats` and `ml`):
- **Full Dashboard UI**: Serves [dashboard.html](cloudops_hub/dashboard.html) featuring live cluster health metrics, responsive telemetry cards, and dynamic chart visualizations.
- **Global Middleware**: Logs every incoming HTTP method and path while tracking total request volume.
- **System Metrics API (`GET /api/metrics`)**: Returns live uptime, node counts, and memory telemetry.
- **Statistical Analytics API (`POST /api/stats`)**: Computes sample mean, variance, standard deviation, and quartiles on user-supplied telemetry datasets.
- **Predictive ML Endpoint (`POST /api/predict`)**: Runs real-time linear regression fit and prediction directly on the server.

**Run Server:**
```bash
spid3r run --engine tree examples/web/cloudops_hub/server.matrix
```

*Or on Windows, simply double-click [run_server.bat](cloudops_hub/run_server.bat).*

Once running, open your browser to **http://127.0.0.1:8090/**.

---

## API Quick Reference

### Using `import "http"`
```spid3r
import "http";

let app = new HttpServer();

// Global middleware
app.before(fn(req) {
    if req["method"] == "OPTIONS" {
        return http_response(204, map("access-control-allow-origin", "*"), "");
    }
    return null; // continue request processing
});

// Routes
app.get("/", fn(req) {
    return http_html(200, "<h1>Welcome to Spid3r</h1>");
});

app.get("/users/:id", fn(req) {
    let id = req["params"]["id"];
    return http_json(200, map("user_id", id, "status", "active"));
});

app.post("/items", fn(req) {
    let parsed = http_body_json(req);
    if parsed.is_err() {
        return http_json(400, map("error", "Invalid JSON payload"));
    }
    return http_json(201, parsed.unwrap());
});

app.listen("127.0.0.1:8080");
```

### Response Helpers
- `http_html(status: int, html: string)`: Returns an HTML response with `Content-Type: text/html`.
- `http_json(status: int, data: any)`: Serializes `data` to JSON with `Content-Type: application/json`.
- `http_response(status: int, headers: map, body: string)`: Constructs a fully customized HTTP response.
- `http_body_json(req: map)`: Parses the incoming request body as JSON into a Result (`ok`/`err`).

---

## Further Documentation
- [STANDARD_LIBRARY.md](../../docs/STANDARD_LIBRARY.md) — Comprehensive guide to standard library modules.
- [docs/html/doc.html](../../docs/html/doc.html) — Interactive language guide (Chapter 16: Concurrency & Networking).
