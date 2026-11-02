# Handoff Report: Milestone M1 (CLI, Manifest & Test Runner Hardening)

## Executive Summary
This investigation formulates the exact hardening and fix strategy for Spid3rWeb's CLI kernel (`ConsoleKernel.matrix`), package manifest (`spider.toml`), and test runner discovery (`tests/run_all.matrix`), enabling `spid3r magic test` and `spid3r test` while preserving full compatibility with `cargo test --test magic_tests` and `cargo test --test web_stack_tests`.

---

## 1. Observations

### 1.1 `spider.toml` Rejection by Spid3r CLI
- **Target File**: `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\spider.toml`
- **Lines 1-6**:
  ```toml
  [package]
  name = "spid3r_web"
  version = "1.0.0"
  entry = "server.matrix"
  description = "Spid3rWeb - A modern full-stack web framework for the Spid3r programming language with Active Record ORM, SpiderBD template engine, expressive routing, and Magic CLI."
  ```
- **Spid3r Manifest Parser**: `C:\Users\needyamin\OneDrive\Desktop\spid3r\src\pkg\mod.rs:242-252`:
  ```rust
  match section.as_str() {
      "package" => {
          let v = parse_string(value).ok_or_else(|| {
              format!("{}: package.{} expects a quoted string", where_, key)
          })?;
          match key.as_str() {
              "name" => name = Some(v),
              "version" => version = Some(v),
              "entry" => entry = Some(v),
              other => return Err(format!("{}: unknown package key '{}'", where_, other)),
          }
      }
  ```
  Only `"name"`, `"version"`, and `"entry"` are recognized in `[package]`. Any other key triggers `Err("unknown package key '...'")`.
- **Observed Command & Verbatim Error**:
  - Command: `cd examples/spid3r_web; ..\..\target\debug\spid3r.exe test`
  - Output:
    ```
    error[usage]: usage error: .\spider.toml: line 5: unknown package key 'description'
    ```
  - Exit code: 1.

### 1.2 `spid3r magic test` Unrecognized Subcommand
- **Target File**: `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\framework\Console\ConsoleKernel.matrix`
- **Lines 35-90**: `handle(self, cli_args)` dispatches `serve`, `route:list`, `migrate`, `migrate:rollback`, `migrate:status`, `db:seed`, `make:controller`, `make:model`, `make:migration`.
  No branch exists for `test`.
- **Lines 98-109**: `print_help(self)` lists commands but omits `test`.
- **Observed Command & Verbatim Error**:
  - Command: `cd examples/spid3r_web; ..\..\target\debug\spid3r.exe magic test`
  - Output:
    ```
    Unknown command: 'test'. Run 'spid3r magic help' for a list of available commands.
    ```
  - Exit code: 1.

### 1.3 `magic_tests.rs` Interface Contracts
- **Target File**: `C:\Users\needyamin\OneDrive\Desktop\spid3r\tests\magic_tests.rs`
- **Lines 50-89**:
  - `spid3r magic help` and `spid3r magic --engine=vm help` assert:
    ```rust
    assert!(out.contains("Spid3rWeb Magic CLI"));
    ```
  - `spid3r magic route:list` from root or `app/` asserts:
    ```rust
    assert!(out.contains("/api/v1/posts"));
    ```
- **Observed Command & Result**:
  - `cargo test --test magic_tests`: Passed cleanly (2 passed; 0 failed).
  - Current `print_help()` in `ConsoleKernel.matrix:94`:
    ```matrix
    println("         Spid3rWeb Magic CLI (Developer Tooling for Spid3r)       ");
    ```
  - Current `command_route_list()` in `ConsoleKernel.matrix:146` prints lines containing `| GET    | /api/v1/posts                      | Closure                        |`.

### 1.4 Test Runner Binary Resolution & Engine Forwarding
- **Target File**: `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\tests\run_all.matrix:9-21`:
  ```matrix
  let spid3r_bin = "spid3r";
  let candidates = list(
      "target/release/spid3r.exe",
      "../../target/release/spid3r.exe",
      "target/release/spid3r",
      "../../target/release/spid3r"
  );
  ```
  It strictly searches `release` directories and does not check `target/debug/`, environment variables (`SPID3R_BIN`, `SPID3R_EXE`, `CARGO_BIN_EXE_spid3r`), or the native `exe_path()` standard library function (`src/stdlib/mod.rs:676-689`).
- **Engine Forwarding**: `spid3r magic` can be invoked with `--engine=vm` or `--engine=tree`. When forwarding to test suites, passing `--engine <engine>` allows differential testing under both engines.
- **Observed Differential Test Result**:
  All 6 test suites (`test_router.matrix`, `test_validator.matrix`, `test_query_builder.matrix`, `test_model.matrix`, `test_bd.matrix`, `test_http_lifecycle.matrix`) pass cleanly and yield 100% byte-for-byte identical output between `--engine tree` and `--engine vm` (177 bytes identical for `test_router.matrix`).

### 1.5 SQLite Artifacts Leftover in Database Directory
- **Observed Directory State**:
  - `examples/spid3r_web/database/database.sqlite-shm` (32,768 bytes)
  - `examples/spid3r_web/database/database.sqlite-wal` (568,592 bytes)
- **Source**: `framework/Database/DB.matrix:280` activates WAL mode:
  `self.db_handle.exec("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;");`
  Without a post-test cleanup or connection closing, these files persist and violate requirement R2 (zero platform-specific execution artifacts).

---

## 2. Logic Chain

1. **Manifest Hardening (`spider.toml`)**:
   - `spid3r test` calls `Manifest::load(Path::new("."))` (`src/cli.rs:1477`).
   - `src/pkg/mod.rs:250` rejects any key in `[package]` other than `name`, `version`, and `entry`.
   - Line 5 of `spider.toml` contains `description = "..."`, causing `spid3r test` to crash immediately with code 1.
   - Converting the description into a comment `# ...` or removing it satisfies `parse_manifest` while preserving human documentation.

2. **CLI Test Runner (`ConsoleKernel.matrix`)**:
   - `magic` forwards command line arguments to `kernel.handle(cli_args)`.
   - When `command == "test"`, `handle()` must branch to `command_test(cli_args)` instead of falling through to the `Unknown command` block.
   - `command_test` must resolve the project root and invoke the framework test suites (`test_router`, `test_validator`, `test_query_builder`, `test_model`, `test_bd`, `test_http_lifecycle`).
   - If all test suites exit 0, `command_test` must return 0; if any fail or if no suites run, it must return 1.
   - `print_help()` must register `test` under `Available commands:` without modifying the header line `"Spid3rWeb Magic CLI"`.

3. **Binary Resolution & Environment Alignment**:
   - `exe_path()` is a built-in native function in Spid3r (`src/stdlib/mod.rs:676`) returning `ok(String)` with the absolute path of the running executable.
   - Checking `exe_path()` first ensures that `spid3r magic test` executes child tests using the exact same binary (debug or release, with or without `.exe`) that launched the CLI.
   - Secondary fallbacks check `SPID3R_BIN`, `SPID3R_EXE`, `CARGO_BIN_EXE_spid3r`, `target/debug/`, `target/release/`, and finally system PATH `"spid3r"`.
   - Setting `SPID3R_PATH` to the project root ensures child processes resolve `framework/...`, `app/...`, and `routes/...` without module not found errors.

4. **Artifact Cleanliness**:
   - Calling `file_delete` (`src/stdlib/mod.rs:3094`) on `database/database.sqlite-shm` and `database/database.sqlite-wal` after test execution guarantees zero leftover database artifacts.

---

## 3. Caveats

- **External CLI changes**: No modifications to the Spid3r language binary or compiler are required; all adjustments are confined to `spider.toml`, `ConsoleKernel.matrix`, and `tests/run_all.matrix`.
- **Database Seed Persistence**: Unit tests in `test_query_builder.matrix` and `test_model.matrix` clean up their created records in their respective test functions; however, running migrations or seeders will modify `database/database.sqlite`. Tests do not depend on external network services.
- **Module Import Scoping**: In Spid3r, `import` statements are resolved statically during semantic checking before `main()` executes. Therefore, setting `SPID3R_PATH` in `command_test` applies to child processes spawned via `exec_capture`, which is the intended execution model.

---

## 4. Conclusion & Proposed Fix Strategy

### 4.1 Fix for `spider.toml`
**File**: `examples/spid3r_web/spider.toml`  
**Action**: Remove the `description = "..."` key from `[package]`. Optionally retain as a comment.

```toml
[package]
name = "spid3r_web"
version = "1.0.0"
entry = "server.matrix"
# Description: Spid3rWeb full-stack web framework with Active Record ORM and SpiderBD

[dependencies]
# Zero external packages required: Spid3r standard library provides
# native HTTP/1.1 networking, SQLite 3 dynamic bindings, Bcrypt password hashing,
# JSON serialization, and thread synchronization.
```

### 4.2 Fix for `framework/Console/ConsoleKernel.matrix`
**File**: `examples/spid3r_web/framework/Console/ConsoleKernel.matrix`

#### Edit 1: Dispatch in `handle()` (insert between `serve` and `route:list`)
```matrix
        if command == "serve" {
            ...
            return self.command_serve(int(port), dry_run);
        } elif command == "test" {
            return self.command_test(cli_args);
        } elif command == "route:list" {
```

#### Edit 2: Registration in `print_help()`
```matrix
    fn print_help(self) {
        println("==================================================================");
        println("         Spid3rWeb Magic CLI (Developer Tooling for Spid3r)       ");
        println("==================================================================");
        println("Usage:");
        println("  spid3r magic <command> [arguments] [options]\n");
        println("Available commands:");
        println("  serve [--port N]          Start the Spid3rWeb local development server");
        println("  test [options]            Run the Spid3rWeb automated test suite");
        println("  route:list                List all registered application routes");
        println("  migrate                   Run outstanding database migrations");
        println("  migrate:rollback          Rollback the last database migration batch");
        println("  migrate:status            Show the status of each migration");
        println("  db:seed                   Seed the database with records");
        println("  make:controller <Name>    Create a new controller class");
        println("  make:model <Name> [-m]    Create a new Active Record model (optional migration)");
        println("  make:migration <name>     Create a new database migration file");
        println("  help                      Display this help message\n");
    }
```

#### Edit 3: Add `resolve_spid3r_binary()`, `cleanup_db_artifacts()`, and `command_test()`
```matrix
    fn resolve_spid3r_binary(self) -> string {
        // 1. Current running executable
        let cur = exe_path();
        if cur.is_ok() {
            let p = cur.unwrap();
            if file_exists(p) {
                return p;
            }
        }

        // 2. Environment variables
        let env_keys = list(
            "SPID3R_BIN",
            "SPID3R_EXE",
            "CARGO_BIN_EXE_spid3r",
            "CARGO_BIN_EXE_spidr"
        );
        for k in env_keys {
            let v = env(k);
            if v.is_some() {
                let p = v.unwrap();
                if file_exists(p) {
                    return p;
                }
            }
        }

        // 3. Search target directories (debug & release)
        let candidates = list(
            "target/debug/spid3r.exe",
            "target/debug/spid3r",
            "../../target/debug/spid3r.exe",
            "../../target/debug/spid3r",
            "target/release/spid3r.exe",
            "target/release/spid3r",
            "../../target/release/spid3r.exe",
            "../../target/release/spid3r"
        );
        for c in candidates {
            if file_exists(c) {
                return c;
            }
        }

        return "spid3r";
    }

    fn cleanup_db_artifacts(self) {
        let root = self.resolve_root();
        let shm_file = root + "database/database.sqlite-shm";
        let wal_file = root + "database/database.sqlite-wal";
        if file_exists(shm_file) {
            file_delete(shm_file);
        }
        if file_exists(wal_file) {
            file_delete(wal_file);
        }
    }

    fn command_test(self, cli_args = null) -> int {
        let root = self.resolve_root();
        if root != "" {
            env_set("SPID3R_PATH", root);
        } else {
            env_set("SPID3R_PATH", ".");
        }

        let spid3r_bin = self.resolve_spid3r_binary();

        // Parse optional arguments: --filter=..., --engine=...
        let filter_substr = "";
        let engine_override = "";
        if cli_args != null {
            let i = 1;
            while i < cli_args.len() {
                let arg = cli_args[i];
                if arg.starts_with("--filter=") {
                    filter_substr = arg.replace("--filter=", "");
                } elif arg == "--filter" and i + 1 < cli_args.len() {
                    i = i + 1;
                    filter_substr = cli_args[i];
                } elif arg.starts_with("--engine=") {
                    engine_override = arg.replace("--engine=", "");
                } elif arg == "--engine" and i + 1 < cli_args.len() {
                    i = i + 1;
                    engine_override = cli_args[i];
                }
                i = i + 1;
            }
        }

        println("==================================================================");
        println("            Spid3rWeb Complete Test Suite Execution               ");
        println("==================================================================");

        let test_suites = list(
            "tests/test_router.matrix",
            "tests/test_validator.matrix",
            "tests/test_query_builder.matrix",
            "tests/test_model.matrix",
            "tests/test_bd.matrix",
            "tests/test_http_lifecycle.matrix"
        );

        let total_run = 0;
        let passed = 0;

        for suite in test_suites {
            if filter_substr != "" and not suite.contains(filter_substr) {
                continue;
            }

            let test_path = suite;
            let candidates = list(
                suite,
                root + suite,
                "examples/spid3r_web/" + suite
            );
            for c in candidates {
                if file_exists(c) {
                    test_path = c;
                    break;
                }
            }

            println("------------------------------------------------------------------");
            println("Running: " + suite);
            println("------------------------------------------------------------------");

            let run_args = list("run");
            if engine_override != "" {
                run_args.push("--engine");
                run_args.push(engine_override);
            }
            run_args.push(test_path);

            let res = exec_capture(spid3r_bin, run_args, null);
            total_run = total_run + 1;

            if res.is_ok() {
                let out = res.unwrap();
                print(out["stdout"]);
                if out["stderr"] != "" {
                    print(out["stderr"]);
                }
                if out["code"] == 0 {
                    passed = passed + 1;
                } else {
                    println("[FAILED]: " + suite + " (exit code " + string(out["code"]) + ")");
                }
            } else {
                println("[EXEC ERROR] " + res.unwrap_err());
                println("[FAILED]: " + suite);
            }
        }

        // Cleanup any SQLite WAL / SHM artifacts generated during DB / Model tests
        self.cleanup_db_artifacts();

        println("==================================================================");
        println("Test Summary: " + string(passed) + "/" + string(total_run) + " test suites passed.");
        println("==================================================================");

        if total_run == 0 or passed < total_run {
            return 1;
        }
        return 0;
    }
```

### 4.3 Complementary Hardening for `tests/run_all.matrix`
**File**: `examples/spid3r_web/tests/run_all.matrix`  
**Action**: Replace `run_test(path)` binary resolution with the hardened multi-tier strategy (checking `exe_path()`, env vars, `target/debug`, `target/release`, `.exe` / no extension), and invoke `file_delete` on SQLite `.sqlite-shm` and `.sqlite-wal` at the conclusion of `main()`.

---

## 5. Verification Method

To independently verify the implementation:

1. **Verify Manifest Standardization**:
   ```powershell
   cd C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web
   $env:SPID3R_PATH = "."
   ..\..\target\debug\spid3r.exe test
   ```
   *Expected Result*: Manifest loads cleanly without `unknown package key 'description'`.

2. **Verify Magic Test CLI Subcommand**:
   ```powershell
   cd C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web
   ..\..\target\debug\spid3r.exe magic test
   ```
   *Expected Result*: Runs all 6 test suites, displays `Test Summary: 6/6 test suites passed.`, exits with code `0`.
   Verify exit code: `$LASTEXITCODE -eq 0`.

3. **Verify Magic Help & Route List Contracts**:
   ```powershell
   cd C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web
   ..\..\target\debug\spid3r.exe magic help
   ..\..\target\debug\spid3r.exe magic route:list
   ```
   *Expected Result*: `magic help` output contains `"Spid3rWeb Magic CLI"` and lists `test [options]`. `magic route:list` contains `"/api/v1/posts"`.

4. **Verify Repository Test Suites**:
   ```powershell
   cd C:\Users\needyamin\OneDrive\Desktop\spid3r
   cargo test --test magic_tests
   cargo test --test web_stack_tests
   ```
   *Expected Result*: 0 test failures across both test suites.

5. **Verify Clean SQLite Artifact State**:
   ```powershell
   Test-Path C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\database\database.sqlite-shm
   Test-Path C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\database\database.sqlite-wal
   ```
   *Expected Result*: Both return `False` after test execution and cleanup.
