# Dispatch: Explorer M1-1 (CLI, Manifest & Test Runner Hardening)

## Working Directory
`C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\explorer_m1_1`

## Inputs
- `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\ORIGINAL_REQUEST.md` (MUST read first)
- `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\PROJECT.md`
- Target Codebase: `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web`

## Milestone M1 Scope
Focus on CLI runner and manifest hardening:
1. `ConsoleKernel.matrix`: analyze implementation of `command_test(cli_args)` to invoke the test runner, handling exit codes (0 for pass, 1 for fail), help registration, and strict preservation of `"Spid3rWeb Magic CLI"` header for `cargo test --test magic_tests`.
2. `spider.toml`: analyze removal of invalid `description` key from `[package]` to fix `spid3r test`.
3. Write your fix strategy to `handoff.md`. DO NOT write or edit source code.

## 2026-11-01T23:17:00Z
You are teamwork_explorer assigned to Milestone M1 (CLI, Manifest & Test Runner Hardening).
Your Working Directory: C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\explorer_m1_1
Read C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\explorer_m1_1\DISPATCH.md and C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\ORIGINAL_REQUEST.md before doing anything else.

Your objective:
Analyze and formulate the exact fix strategy for:
1. Implementing command_test in ConsoleKernel.matrix to run the test suite and exit with code 0 on pass or 1 on fail, registered in handle() and print_help().
2. Preserving the exact "Spid3rWeb Magic CLI" help header and route listing format for cargo test --test magic_tests compatibility.
3. Standardizing spider.toml by removing description from [package].
Write your findings and fix strategy to C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\explorer_m1_1\handoff.md and notify me when complete. DO NOT modify source files directly.
