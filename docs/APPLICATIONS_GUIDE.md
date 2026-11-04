# Spid3r Native GUI & Web Application Showcase

This document details two complete, production-grade applications written in the **Spid3r** programming language, utilizing its first-party standard library (`stats.matrix`, `ml.matrix`, `web.matrix`, `http.matrix`, and the native GUI subsystem).

---

## 1. Native GUI Application: DataLab & Analytics Studio

- **Directory**: `examples/gui/data_studio/`
- **Source Script**: [`app.matrix`](../examples/gui/data_studio/app.matrix)
- **UI Frontend**: [`index.html`](../examples/gui/data_studio/index.html)
- **Batch Launcher**: [`run_gui.bat`](../examples/gui/data_studio/run_gui.bat)

### Architecture
- Runs on the Spid3r Tree Interpreter with Tao window and Wry WebView bridge (`spid3r gui <file.matrix> --html <index.html>`).
- Bidirectional event bridge:
  - Frontend emits user interaction events: `window.spid3r.emit(event, payload)`.
  - Spid3r registers native event listeners: `gui_on("compute_stats", on_compute_stats)`, `gui_on("regression", on_regression)`, `gui_on("matrix_op", on_matrix_op)`.
  - Spid3r executes native statistical and machine learning algorithms and pushes live state updates to the UI via `gui_eval(...)`.

### Features
1. **Statistical Analyzer**: Computes sample size, mean ($\mu$), median, variance ($\sigma^2$), standard deviation ($\sigma$), quartiles, and IQR using `stdlib/stats.matrix`. Renders an interactive histogram on Canvas.
2. **Machine Learning Linear Regression**: Performs closed-form normal equation solving using `stdlib/ml.matrix`. Fits slope, intercept, computes $R^2$ score, dynamically plots regression line on Canvas, and provides a custom coordinate predictor.
3. **Matrix 2x2 Decomposition**: Computes determinant, trace, eigenvalues, and matrix inversion using Spid3r math kernels.

### Running the GUI App
```powershell
# Interactive desktop window
.\target\debug\spid3r.exe gui examples\gui\data_studio\app.matrix --html examples\gui\data_studio\index.html

# Deterministic headless verification (no window)
.\target\debug\spid3r.exe gui examples\gui\data_studio\app.matrix --html examples\gui\data_studio\index.html --dry-run
```

---

## 2. Web Application: Spid3rWeb Blog Platform

- **Directory**: `examples/spid3r_web/`
- **Server Script**: [`server.matrix`](../examples/spid3r_web/server.matrix)
- **Full guide**: [docs/html/spid3rweb.html](html/spid3rweb.html)
- **URL**: [http://127.0.0.1:8000/](http://127.0.0.1:8000/) (default port 8000; override by passing a port as the first CLI argument)

### Architecture
- Powered by **Spid3rWeb**, the full-stack web framework that ships in `examples/spid3r_web/framework/` (see `docs/html/spid3rweb.html`).
- Expressive router (`Route.get`/`Route.post`) with parameterized paths (`/posts/:id`), route names, and middleware guards.
- Active Record ORM on native SQLite (WAL mode), transactional migrations and seeders (`database/`).
- SpiderBD template engine: `.bd.html` views in `resources/views/` with `@extends`, `@section`, `@foreach`, `@if`, and `{{ $var }}` interpolation.
- Built-in authentication: bcrypt password hashing (`bcrypt_hash` / `bcrypt_verify`), session tokens, and cookie headers.
- `magic` CLI (`spid3r magic <command>`) for migrations, scaffolding, and the dev server, driven by the extensionless `magic` root file.

### Routes
| Method | Path | Description |
|---|---|---|
| `GET` | `/` | Home page. |
| `GET` | `/posts` | List all posts. |
| `GET` | `/posts/create` · `POST` `/posts` | Create a post (auth middleware). |
| `GET` | `/posts/:id` | Show a single post. |
| `GET` | `/posts/:id/edit` · `POST` `/posts/:id/update` | Edit and update a post (auth middleware). |
| `POST` | `/posts/:id/delete` | Delete a post (auth middleware). |
| `GET` | `/login` · `POST` `/login` | Log in. |
| `GET` | `/register` · `POST` `/register` | Register a new user. |
| `POST` | `/logout` | Log out. |
| `GET` | `/status` | JSON health/status endpoint (API routes). |

### Running the Web Server
```powershell
# From examples/spid3r_web/ — start the app server on port 8000
.\..\..\target\debug\spid3r.exe run server.matrix

# Or with an explicit port
.\..\..\target\debug\spid3r.exe run server.matrix -- 8080
```
Then navigate to [http://127.0.0.1:8000/](http://127.0.0.1:8000/) in any modern web browser.
