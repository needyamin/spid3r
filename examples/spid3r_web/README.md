# Spid3rWeb 🕸️

**Spid3rWeb** is an expressive, full-stack web framework engineered specifically for the [Spid3r programming language](https://github.com/needyamin/spid3r).

Spid3rWeb delivers elegant syntax, developer productivity, and clean architectural design directly to Spid3r, operating with **zero external dependencies** and running natively on Spid3r's high-performance virtual machine and AOT compiler.

---

## ✨ Features

- **Active Record ORM**: Fluent SQL query builder (`DB.table(...)`), automatic timestamps (`created_at`, `updated_at`), mass-assignment protection (`fillable`), and relationship accessors (`user.posts()`, `post.author()`).
- **Transactional Migrations & Seeders**: Declarative schema definition (`Schema.create(...)`), automatic migration tracking (`migrations` table), rollback capabilities, and database seeders.
- **SpiderBD Template Engine**: Modular view layout system supporting `.bd.html` templates with `@extends`, `@section`, `@yield`, `@include`, conditional blocks (`@if`/`@else`/`@endif`), loops (`@foreach`), and variable interpolation (`{{ $var }}` escaped, `{!! $raw !!}` unescaped).
- **Expressive Router**: Route registration (`Route.get`, `Route.post`), parameterized path matching (`/posts/:id`), route groups with prefix inheritance (`Route.group`), and route table inspection (`route:list`).
- **HTTP Kernel & Middleware Pipeline**: Onion-architecture middleware runner (`Pipeline`), global CORS & request logging, authenticated session guards, and custom error pages (404 Not Found, 500 Server Error).
- **Native SQLite Persistence**: High-throughput embedded persistence utilizing Spid3r's native SQLite C driver with WAL mode and foreign key constraints.
- **Built-in Authentication**: Secure user authentication powered by native Bcrypt hashing (`bcrypt_hash`, `bcrypt_verify`), session tokens, and cookie headers.
- **Magic CLI**: Comprehensive developer command-line interface (`spid3r magic <command>`) powered by a single extensionless root file `magic` for migrations, code scaffolding, routing inspection, and development servers.

---

## 📁 Directory Structure

```text
examples/spid3r_web/
├── app/
│   ├── Controllers/             # MVC Controllers
│   │   ├── AuthController.matrix
│   │   ├── HomeController.matrix
│   │   └── PostController.matrix
│   ├── Middleware/              # Application Middleware
│   └── Models/                  # Active Record Models
│       ├── Post.matrix
│       └── User.matrix
├── config/
│   ├── app.matrix               # App environment, URL, and debug settings
│   └── database.matrix          # SQLite connection and pragma configuration
├── database/
│   ├── database.sqlite          # SQLite database storage
│   ├── migrations/              # Schema migration files
│   │   ├── 001_create_users_table.matrix
│   │   ├── 002_create_posts_table.matrix
│   │   └── 003_create_sessions_table.matrix
│   └── seeders/                 # Database seeders
│       └── DatabaseSeeder.matrix
├── framework/                   # Spid3rWeb Framework Core
│   ├── Application.matrix       # Application kernel & HTTP server bridge
│   ├── Controller.matrix        # Controller base class
│   ├── Middleware.matrix        # Pipeline & middleware definitions
│   ├── Request.matrix           # Request wrapper & validation engine
│   ├── Response.matrix          # Response builder & helper functions
│   ├── Router.matrix            # Expressive router & route matching
│   ├── Auth/                    # Authentication & Hashing facades
│   │   ├── Auth.matrix
│   │   └── Hash.matrix
│   ├── Console/                 # Magic CLI command kernel
│   │   └── ConsoleKernel.matrix
│   ├── Database/                # Active Record ORM, QueryBuilder & Migrations
│   │   ├── DB.matrix
│   │   ├── Migration.matrix
│   │   ├── Model.matrix
│   │   └── Schema.matrix
│   └── View/                    # SpiderBD template compiler
│       └── BD.matrix
├── public/                      # Static web assets
│   ├── css/
│   │   └── app.css
│   └── js/
│       └── app.js
├── resources/
│   └── views/                   # SpiderBD templates (.bd.html)
│       ├── home.bd.html
│       ├── auth/
│       │   ├── login.bd.html
│       │   └── register.bd.html
│       ├── layouts/
│       │   └── app.bd.html
│       └── posts/
│           ├── create.bd.html
│           ├── edit.bd.html
│           ├── index.bd.html
│           └── show.bd.html
├── routes/
│   ├── api.matrix               # JSON REST API routes
│   └── web.matrix               # Web frontend routes
├── tests/                       # Automated test suites
│   ├── run_all.matrix           # Master test runner
│   ├── test_bd.matrix           # SpiderBD engine unit tests
│   ├── test_http_lifecycle.matrix # HTTP lifecycle integration tests
│   ├── test_model.matrix        # Active Record model unit tests
│   ├── test_query_builder.matrix# Query builder unit tests
│   ├── test_router.matrix       # Router unit tests
│   └── test_validator.matrix    # Validation engine unit tests
├── magic                        # Single root Magic CLI entry point (no extension)
├── server.matrix                # Production / Dev HTTP server entry point
└── spider.toml                  # Spid3r package manifest
```

---

## 🚀 Getting Started

### 1. Requirements

- Spid3r compiler & runtime toolchain (`spid3r` on PATH or built in repository `target/release/spid3r`).
- Windows, Linux, or macOS.

### 2. Database Migrations & Seeding

Run database migrations to initialize the schema:

```bash
spid3r magic migrate
```

Seed initial demo data (including user `ada@example.com` and initial blog articles):

```bash
spid3r magic db:seed
```

Check migration status:

```bash
spid3r magic migrate:status
```

### 3. Start Development Server

Start the built-in HTTP server on default port `8000`:

```bash
spid3r magic serve
```

Or customize the listening port:

```bash
spid3r magic serve --port 8080
```

Open your browser at **[http://localhost:8000](http://localhost:8000)**.

---

## 🛠️ Magic CLI Commands

| Command | Description |
|---|---|
| `spid3r magic serve [--port N]` | Start the local HTTP web server |
| `spid3r magic route:list` | Display a formatted ASCII table of all registered routes |
| `spid3r magic migrate` | Run outstanding database migrations |
| `spid3r magic migrate:rollback` | Roll back the most recent migration batch |
| `spid3r magic migrate:status` | Check which migrations have been applied |
| `spid3r magic db:seed` | Seed the database with demo users and articles |
| `spid3r magic make:controller <Name>` | Scaffold a new Controller class in `app/Controllers/` |
| `spid3r magic make:model <Name> [-m]` | Scaffold a new Model in `app/Models/` (optionally with migration) |
| `spid3r magic make:migration <name>` | Scaffold a timestamped schema migration file |
| `spid3r magic help` | Show available Magic commands |

---

## 📖 Framework Usage

### Defining Routes (`routes/web.matrix`)

```spid3r
import "framework/Router";

Route.get("/", fn(req) {
    return view("home", map("title", "Welcome"));
});

Route.get("/posts/:id", fn(req) {
    let post_id = int(req.param("id"));
    let post = PostModel.find(post_id);
    return view("posts.show", map("post", post.to_map()));
});

Route.group(map("prefix", "/admin", "middleware", "auth"), fn() {
    Route.get("/dashboard", fn(req) {
        return view("admin.dashboard");
    });
});
```

### Active Record Models (`app/Models/User.matrix`)

```spid3r
import "framework/Database/Model";
import "framework/Database/DB";

public class User {
    public let model;

    fn new(attrs = null) {
        self.model = new Model("users", list("name", "email", "password"), attrs);
    }

    public fn save(self) { return self.model.save(); }
    public fn get(self, key) { return self.model.get(key); }
    public fn to_map(self) { return self.model.to_map(); }

    public fn posts(self) {
        return DB.table("posts").where("user_id", "=", self.get("id")).get();
    }
}
```

### Fluent Query Builder

```spid3r
import "framework/Database/DB";

// Select with fluent constraints
let articles = DB.table("posts")
    .where("status", "=", "published")
    .orderBy("id", "DESC")
    .limit(10)
    .get();

// Insert
let id = DB.table("posts").insert(map(
    "title", "Hello World",
    "content", "Full stack framework in Spid3r",
    "user_id", 1
));

// Update
DB.table("posts").where("id", "=", id).update(map("title", "Updated Title"));

// Count
let count = DB.table("posts").count();
```

### SpiderBD Templates (`resources/views/posts/index.bd.html`)

```html
@extends('layouts/app')

@section('title', 'All Articles')

@section('content')
<div class="container py-4">
  <h2 class="text-light fw-bold mb-4">Published Articles</h2>

  @foreach(posts as post)
  <div class="card bg-dark border-secondary mb-3">
    <div class="card-body">
      <h4 class="card-title text-info">{{ $post.title }}</h4>
      <p class="text-secondary">{{ $post.content }}</p>
      <a href="/posts/{{ $post.id }}" class="btn btn-outline-info btn-sm">Read &rarr;</a>
    </div>
  </div>
  @endforeach
</div>
@endsection
```

### Request Validation

```spid3r
public fn store(self, req) {
    let validator = req.validate(map(
        "title", "required|min:5",
        "email", "required|email",
        "content", "required|min:20"
    ));

    if validator.fails() {
        return view("posts.create", map(
            "error", validator.first_error("title")
        ));
    }

    // Proceed with validated inputs...
}
```

---

## 🧪 Automated Testing

Spid3rWeb includes comprehensive test coverage spanning router matching, request validation, query building, active record persistence, template rendering, and full HTTP lifecycle:

```bash
spid3r run tests/run_all.matrix
```

All 6 test suites execute with 100% pass rate:

```text
==================================================================
            Spid3rWeb Complete Test Suite Execution               
==================================================================
[PASS] test_router.matrix          (Route matching, params, groups)
[PASS] test_validator.matrix       (Required, email, min, max, numeric)
[PASS] test_query_builder.matrix   (Fluent SQL, CRUD, transactions)
[PASS] test_model.matrix           (Active Record, timestamps, relations)
[PASS] test_bd.matrix              (Layouts, sections, yields, foreach)
[PASS] test_http_lifecycle.matrix  (Full 200, 404, 401 dispatch)
==================================================================
Test Summary: 6/6 test suites passed.
==================================================================
```

---

## 📄 License

GPL-3.0-or-later. Built with ❤️ for the Spid3r programming language ecosystem.
