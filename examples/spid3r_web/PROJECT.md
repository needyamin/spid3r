# Project: Spid3rWeb Modernization & Hardening

## Architecture
- **Layer 1: Runtime & CLI**: `magic` entrypoint, `ConsoleKernel.matrix`, `spider.toml` package manifest. Provides command dispatch (`serve`, `migrate`, `db:seed`, `test`, `route:list`, `make:*`).
- **Layer 2: Database & ORM**: `DB.matrix` (QueryBuilder), `Schema.matrix`, `Migration.matrix`, `Model.matrix` (Active Record base class with Spid3r 2.0 `class Sub extends Model` and `public static fn`), domain models (`User`, `Post`).
- **Layer 3: HTTP Foundation & Routing**: `Request.matrix` (parsing, URL decoding, validation rules), `Response.matrix` (headers, status codes, cookies, JSON/HTML builders), `Router.matrix` (pattern matching, parameter extraction, grouping).
- **Layer 4: Middleware Pipeline**: `Middleware.matrix` (pipeline runner, onion wrapping), `Application.matrix` (HTTP server lifecycle, global middleware execution, static file security), built-in middlewares (`CorsMiddleware`, `LoggerMiddleware`, `AuthMiddleware`).
- **Layer 5: Presentation / View Engine**: `BD.matrix` (SpiderBD `.bd.html` compiler, sections, yields, layouts, nested dot-notation evaluation `{{ obj.prop }}` and `{!! obj.prop !!}`).
- **Layer 6: Security & Authentication**: `Auth.matrix` (CSPRNG session tokens, session persistence, login/logout), `Hash.matrix` (bcrypt password hashing).

## Feature Inventory
| # | Feature | Description | Milestone | Source |
|---|---------|-------------|-----------|--------|
| 1 | Magic Test Subcommand | Add `command_test` to `ConsoleKernel.matrix` to execute full test suite via `spid3r magic test` with exit 0/1 | M1 | Survey 1, 3 |
| 2 | Magic Help & Contract Output | Preserve `"Spid3rWeb Magic CLI"` header and `route:list` containing `"/api/v1/posts"` for `cargo test --test magic_tests` | M1 | Survey 3 |
| 3 | Package Manifest Standardization | Remove unsupported `description` key from `[package]` in `spider.toml` to fix `spid3r test` | M1 | Survey 1, 3 |
| 4 | Test Runner Binary Resolution | Hardened test runner searching `target/debug/`, `target/release/`, env vars, without hardcoding `.exe`, with engine forwarding | M1 | Survey 3 |
| 5 | Clean DB Artifact Management | Eliminate leftover SQLite `.sqlite-shm` and `.sqlite-wal` files; ensure clean connection closing or in-memory test DB | M1 | Survey 1, 3 |
| 6 | Active Record Model Inheritance | Refactor `User` and `Post` to inherit from `Model` (`class User extends Model`) using `super(...)` | M2 | Survey 1, 2 |
| 7 | Static Model Methods | Implement `public static fn find()`, `all()`, `create()`, etc. on models; maintain backwards-compatible aliases | M2 | Survey 1, 2 |
| 8 | Migration Class Inheritance | Refactor migrations (`001_create_users_table`, etc.) to inherit from `Migration` base class | M2 | Survey 1 |
| 9 | QueryBuilder Joins Compilation | Compile `self.joins` in `DB.matrix` `build_select_sql()` for SQL join queries | M2 | Survey 1 |
| 10 | Dot-Notation Variable Resolution | Support nested variable access (`{{ post.title }}`, `{!! user.name !!}`) in `BD.matrix` outside `@foreach` | M3 | Survey 1 |
| 11 | BD Whitespace & Syntax Tolerance | Handle flexible spacing in template tags (`{{$var}}`, `{{ $var }}`, `{{ var }}`) and safe HTML escaping | M3 | Survey 1 |
| 12 | Layout & View Template Polish | Modernize view templates (`resources/views/`) using robust variable interpolation and modern Spid3r idioms | M3 | Survey 1 |
| 13 | Static File Path Traversal Defense | Fix `try_serve_static()` in `Application.matrix` to sanitize paths and reject `../` traversal attacks | M4 | Survey 1 |
| 14 | HTTP Method 405 Handling | Enhance `Router.matrix` and `Application.matrix` to return 405 Method Not Allowed when path matches another verb | M4 | Survey 1 |
| 15 | Response Cookie Security | Set `HttpOnly`, `SameSite=Lax`, `Secure` flags in `Response.matrix:cookie()` and support multi-cookie responses | M4 | Survey 1, 3 |
| 16 | Request Parsing & Binary Safety | Ensure binary file uploads read `req["body_bytes"]` and URL decoding handles all RFC edge cases | M4 | Survey 2, 3 |
| 17 | Global Middleware Execution Fix | Fix `Application.matrix` lines 89-92 to properly execute global middlewares through `.then()` on incoming requests | M5 | Survey 1 |
| 18 | Dynamic Middleware Registry | Support named middleware registration and resolution in `Middleware.matrix` and `Application.matrix` | M5 | Survey 1 |
| 19 | Cryptographic Session Tokens | Replace predictable millisecond session tokens in `Auth.matrix` with CSPRNG `random_bytes` / `uuid_v4()` | M5 | Survey 1, 2 |
| 20 | Session Database Schema Alignment | Align `003_create_sessions_table.matrix` schema with `Auth.matrix` token column storage and lookup | M5 | Survey 1 |
| 21 | Modern Syntax & Idioms | Modernize collection literals (`{}` / `[]`), arrow closures (`=>`), and string interpolation throughout codebase | M1-M5 | Survey 2 |
| 22 | E2E Opaque-Box Test Suite | Comprehensive 4-tier test suite verifying all features independently from requirements | E2E Track | Architecture |
| 23 | Final E2E Verification & Hardening | Pass 100% of E2E tests, pass repository cargo tests, and perform Tier 5 adversarial coverage hardening | M6 | Architecture |

## Milestones
| # | Name | Scope | Dependencies | Status |
|---|------|-------|-------------|--------|
| M1 | CLI, Manifest & Test Runner Hardening | `ConsoleKernel.matrix` (`test` command, help header), `spider.toml`, test runner discovery, SQLite artifact cleanup | none | DONE (Passed: magic test 6/6, magic_tests 2/2, web_stack_tests 8/8, tier1_features_cli 15/15, 0 artifacts) |
| M2 | Active Record ORM & Persistence | `Model.matrix`, `DB.matrix` (joins, busy_timeout), `User.matrix`, `Post.matrix` (`extends`, `static fn`), migrations (`extends`) | M1 | DONE (Passed: magic test 6/6, test_model 2/2, test_query_builder 4/4, tier1_features_orm 15/15, dual-engine tree==vm, cargo tests pass, CLEAN audit) |
| M3 | SpiderBD Template Engine Modernization | `BD.matrix` (nested dot-notation `post.title`, whitespace flexibility), view templates verification | M1 | PLANNED |
| M4 | Router, Request/Response Lifecycle & Security | `Router.matrix` (405 matching), `Request.matrix`, `Response.matrix` (cookies), `Application.matrix` (traversal fix) | M1 | PLANNED |
| M5 | Middleware Pipeline & Authentication Hardening | `Application.matrix` (global pipeline execution fix), `Middleware.matrix` (registry), `Auth.matrix` (CSPRNG, schema alignment) | M2, M4 | PLANNED |
| M6 | Final E2E Test Pass & Adversarial Hardening | Pass 100% E2E tests (Tiers 1-4), verify cargo tests (`magic_tests`, `web_stack_tests`), Tier 5 adversarial hardening | M1-M5, E2E Track | PLANNED |
| E2E | E2E Testing Track | Independent opaque-box test infra and test cases (Tiers 1-4); publishes `TEST_READY.md` | none (runs in parallel) | DONE (192 tests authored in tests/e2e/, TEST_READY.md published) |

## Interface Contracts
### CLI ↔ Test Runner
- `spid3r magic test`: Exits with code 0 if all test suites pass, non-zero code if any test suite fails.
- `spid3r magic help`: Must include `"Spid3rWeb Magic CLI"` in stdout.
- `spid3r magic route:list`: Must include `"/api/v1/posts"` in stdout.

### Model ↔ Active Record Base
- `class User extends Model`, `class Post extends Model`.
- `public static fn find(id) -> Model?`, `public static fn all() -> list`, `public static fn create(attrs) -> Model`.
- Backwards compatibility: `UserModel` and `PostModel` aliases resolve to model classes or instances.

### View Engine ↔ Data Models / Maps
- `BD.matrix` `render(template, data)` resolves top-level variables `{{ var }}` and nested properties `{{ obj.prop }}`, `{{ obj.nested.prop }}` for both `map` and `Model` instances.
- `{!! expr !!}` outputs unescaped raw HTML; `{{ expr }}` outputs escaped HTML via `html_escape`.

### Pipeline ↔ Application ↔ Middleware
- `Application.handle_request(req)` executes `global_middleware` pipeline via `then()`, resolving route and route-specific middleware.
- Middleware returns `Response` or `null` (or passes to next handler).

### Auth ↔ Sessions Table
- Sessions table stores unique session token in column `token: string`, user ID in `user_id: int`, and timestamp in `last_activity: int`.
- `Auth.login(user)` generates CSPRNG token (`sess_...`), inserts into `sessions`, and sets cookie `spid3r_session`.

## Code Layout
- `examples/spid3r_web/framework/`: Core framework classes (`Application.matrix`, `Controller.matrix`, `Middleware.matrix`, `Request.matrix`, `Response.matrix`, `Router.matrix`).
- `examples/spid3r_web/framework/Console/`: CLI kernel (`ConsoleKernel.matrix`).
- `examples/spid3r_web/framework/Database/`: ORM & QueryBuilder (`DB.matrix`, `Model.matrix`, `Schema.matrix`, `Migration.matrix`).
- `examples/spid3r_web/framework/View/`: Template engine (`BD.matrix`).
- `examples/spid3r_web/framework/Auth/`: Authentication & hashing (`Auth.matrix`, `Hash.matrix`).
- `examples/spid3r_web/app/`: Application domain (`Controllers/`, `Models/`, `Middleware/`).
- `examples/spid3r_web/routes/`: Route definitions (`web.matrix`, `api.matrix`).
- `examples/spid3r_web/resources/views/`: SpiderBD templates (`.bd.html`).
- `examples/spid3r_web/tests/`: Framework automated test suites.
- `examples/spid3r_web/spider.toml`: Package manifest.
- `examples/spid3r_web/magic`: CLI entrypoint.
