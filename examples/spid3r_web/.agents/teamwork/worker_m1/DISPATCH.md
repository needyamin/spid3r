# Dispatch: Worker M1 (CLI, Manifest & Test Runner Hardening)

## Working Directory
`C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\worker_m1`

## Mandatory Integrity Warning
DO NOT CHEAT. All implementations must be genuine. DO NOT hardcode test results, create dummy/facade implementations, or circumvent the intended task. A teamwork_auditor will independently verify your work. Integrity violations WILL be detected and your work WILL be rejected.

## Inputs
- `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\ORIGINAL_REQUEST.md` (MUST read first)
- `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\PROJECT.md`
- `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\explorer_m1_1\handoff.md`
- `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\explorer_m1_3\handoff.md`

## Files Owned Exclusively
- `examples/spid3r_web/spider.toml`
- `examples/spid3r_web/framework/Console/ConsoleKernel.matrix`
- `examples/spid3r_web/framework/Database/DB.matrix`
- `examples/spid3r_web/tests/run_all.matrix`
- `examples/spid3r_web/tests/test_query_builder.matrix`
- `examples/spid3r_web/tests/test_model.matrix`
- `examples/spid3r_web/tests/test_http_lifecycle.matrix`
- `examples/spid3r_web/database/` (cleaning untracked `.sqlite-shm` and `.sqlite-wal`)

## Tasks to Implement
1. `spider.toml`: Remove `description = "..."` from `[package]` to fix `spid3r test`.
2. `framework/Database/DB.matrix`:
   - Implement `checkpoint(self)` (`PRAGMA wal_checkpoint(TRUNCATE);`).
   - Implement `cleanup_artifacts(self)` to delete `<path>-shm` and `<path>-wal` if present.
   - Update `close(self)` to checkpoint and call `cleanup_artifacts()`.
3. `framework/Console/ConsoleKernel.matrix`:
   - Add `command_test(self, cli_args)` that resolves the binary via `exe_path()`, env vars, target dirs, runs the test suites, forwards `--engine` and `--filter`, runs `cleanup_artifacts()`, and returns 0 on pass or 1 on fail.
   - Dispatch `test` in `handle(self, cli_args)`.
   - Register `test [options]` in `print_help()`, STRICTLY PRESERVING `"Spid3rWeb Magic CLI"` header and route list formatting (`/api/v1/posts`).
   - Call `DB.close()` in `command_migrate`, `command_rollback`, `command_migrate_status`, `command_db_seed`, and `command_serve`.
4. `tests/run_all.matrix`: Harden binary resolution with `exe_path()`, env vars, `target/debug/` and `target/release/` candidates, and invoke cleanup at the end.
5. Add `DB.close()` teardown in `tests/test_query_builder.matrix`, `tests/test_model.matrix`, and `tests/test_http_lifecycle.matrix`.
6. Delete untracked files `database/database.sqlite-shm` and `database/database.sqlite-wal`.
7. Verify all builds and tests:
   - `spid3r magic test` from `examples/spid3r_web` passes with exit code 0.
   - `cargo test --test magic_tests` from repository root passes (2 passed, 0 failed).
   - `cargo test --test web_stack_tests` from repository root passes (8 passed, 0 failed).
   - Confirm zero `.sqlite-shm` or `.sqlite-wal` files left on disk.
8. Write comprehensive completion report to `handoff.md`.


## 2026-11-01T23:28:19Z
You are teamwork_worker assigned to implement Milestone M1 (CLI, Manifest & Test Runner Hardening).
Your Working Directory: C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\worker_m1
Read C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\worker_m1\DISPATCH.md and C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\ORIGINAL_REQUEST.md before doing anything else.

MANDATORY INTEGRITY WARNING:
DO NOT CHEAT. All implementations must be genuine. DO NOT hardcode test results, create dummy/facade implementations, or circumvent the intended task. A teamwork_auditor will independently verify your work. Integrity violations WILL be detected and your work WILL be rejected.

Your objectives:
1. Implement all changes described in DISPATCH.md across the files you own exclusively (spider.toml, ConsoleKernel.matrix, DB.matrix, run_all.matrix, test teardowns, and database artifact cleanup).
2. Run build and test verification:
   - Run `spid3r magic test` (or using spid3r.exe) in examples/spid3r_web. Verify all 6 suites pass.
   - Run `cargo test --test magic_tests` in repository root. Verify 2/2 pass.
   - Run `cargo test --test web_stack_tests` in repository root. Verify 8/8 pass.
   - Verify database/database.sqlite-shm and database.sqlite-wal are gone.
3. Document all edits and verification outputs in your handoff report at C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\worker_m1\handoff.md and notify me when complete.
