# Dispatch Task: Survey Repository Test Suites and Verification Contracts

## Working Directory
`C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\explorer_survey_3`

## Inputs
- `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\ORIGINAL_REQUEST.md` (MUST read first)
- Spid3r root repository: `C:\Users\needyamin\OneDrive\Desktop\spid3r`
- Target codebase: `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web`

## Objectives
1. Read `ORIGINAL_REQUEST.md`.
2. Inspect `cargo test --test magic_tests` and `cargo test --test web_stack_tests` in `C:\Users\needyamin\OneDrive\Desktop\spid3r`.
3. Understand how these test suites interact with `spid3r_web`, what CLI commands they invoke, what files/paths they expect, what assertions they make, and what current tests exist.
4. Inspect `spid3r magic test` execution mechanism in `c:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web`.
5. Identify any platform-specific execution artifacts, wrapper binaries, temporary files, or environment assumptions that violate R2 ("zero platform-specific execution artifacts or wrapper binaries").
6. Write your comprehensive verification survey report to `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\explorer_survey_3\handoff.md`.


## 2026-11-01T23:01:14Z
You are teamwork_explorer assigned to inspect repository test suites and verification contracts.
Your Working Directory: C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\explorer_survey_3
Read C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\explorer_survey_3\DISPATCH.md and C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\ORIGINAL_REQUEST.md before doing anything else.

Your objective:
Investigate cargo tests (cargo test --test magic_tests and cargo test --test web_stack_tests in C:\Users\needyamin\OneDrive\Desktop\spid3r) and the framework test runner (spid3r magic test).
Determine how tests are structured, what assertions they make, how they execute commands, what causes test failures or fragility, and identify any platform-specific execution artifacts/wrappers violating R2.
Write your report to C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\explorer_survey_3\handoff.md and update progress.md regularly. When finished, send a completion message back with the path to your handoff.md.
