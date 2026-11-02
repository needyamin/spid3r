# Spid3rWeb E2E Test Suite Specification & Execution Guide

**Published At**: 2026-11-01T23:39:00Z  
**Track**: E2E Testing Track (Independent, Opaque-Box)  
**Status**: COMPLETE & VERIFIED  

---

## 1. Quick Start / Test Runner Commands

All commands are executed from the Spid3rWeb project root: `c:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web`

### Primary 4-Tier E2E Master Runner
```powershell
$env:SPID3R_PATH = '.'
spid3r run tests/e2e/run_e2e.matrix
```

### Strict Mode E2E Runner (Non-zero exit if any suite fails)
```powershell
$env:SPID3R_PATH = '.'
spid3r run tests/e2e/run_e2e.matrix --strict
```

### Magic CLI Test Command
```powershell
spid3r magic test
```

### Individual Test Suite Runners
```powershell
# Tier 1: Core Feature Coverage (85 tests across 17 features)
spid3r run tests/e2e/tier1_features_cli.matrix          # Features 1, 2, 3 (15 tests)
spid3r run tests/e2e/tier1_features_orm.matrix          # Features 4, 5, 6 (15 tests)
spid3r run tests/e2e/tier1_features_http.matrix         # Features 7, 8, 9, 10, 11 (25 tests)
spid3r run tests/e2e/tier1_features_advanced.matrix     # Features 12, 13, 14, 15, 16, 17 (30 tests)

# Tier 2: Boundary & Corner Cases (85 tests across 17 features)
spid3r run tests/e2e/tier2_boundaries_part1.matrix      # Boundaries for Features 1-9 (45 tests)
spid3r run tests/e2e/tier2_boundaries_part2.matrix      # Boundaries for Features 10-17 (40 tests)

# Tier 3: Cross-Feature Pairwise Interactions (17 tests)
spid3r run tests/e2e/tier3_cross_feature.matrix         # 17 pairwise integration combinations

# Tier 4: Real-World Application Scenarios (5 workflows)
spid3r run tests/e2e/tier4_real_world.matrix            # 5 end-to-end user & API workflows
```

---

## 2. Test Coverage Breakdown

The E2E test suite adheres to the 4-tier requirement-driven methodology specified in `TEST_INFRA.md`:

| Tier | Category | File Path | Test Count | Status |
|:---:|---|---|:---:|:---:|
| **Tier 1** | CLI & Manifest (F1, F2, F3) | `tests/e2e/tier1_features_cli.matrix` | 15 | PASS (15/15) |
| **Tier 1** | ORM, Relations & DDL (F4, F5, F6) | `tests/e2e/tier1_features_orm.matrix` | 15 | PASS (15/15) |
| **Tier 1** | HTTP, Routing & Validation (F7-F11) | `tests/e2e/tier1_features_http.matrix` | 25 | 24 PASS / 1 FAIL* |
| **Tier 1** | Middleware, BD, Auth & Static (F12-F17) | `tests/e2e/tier1_features_advanced.matrix` | 30 | 26 PASS / 4 FAIL* |
| **Tier 2** | Boundaries Part 1 (F1-F9) | `tests/e2e/tier2_boundaries_part1.matrix` | 45 | 44 PASS / 1 FAIL* |
| **Tier 2** | Boundaries Part 2 (F10-F17) | `tests/e2e/tier2_boundaries_part2.matrix` | 40 | 39 PASS / 1 FAIL* |
| **Tier 3** | Pairwise Combinations | `tests/e2e/tier3_cross_feature.matrix` | 17 | PASS (17/17) |
| **Tier 4** | Real-World Scenarios | `tests/e2e/tier4_real_world.matrix` | 5 | 4 PASS / 1 FAIL* |
| **TOTAL** | **Complete E2E Suite** | **8 Test Suites** | **192** | **184 PASS (95.8%)** |

*\*Note on Failing Tests: The 8 currently failing assertions test the exact pending feature additions scheduled for Milestones M3, M4, and M5 as cataloged below.*

---

## 3. Pending Feature Mapping (Milestones M3, M4, M5)

The 8 failing tests act as automated acceptance criteria for the remaining implementation milestones:

1. **Milestone 3 (SpiderBD Modernization)**:
   - `F13.4`: Support nested variable access (`{{ post.title }}`) outside `@foreach` in `BD.matrix`.
   - `F13.5`: Flexible spacing tolerance in template tags (`{{$var}}`, `{{ $var }}`).
2. **Milestone 4 (Router & Security Hardening)**:
   - `F8.5` & `B8.5`: HTTP 405 Method Not Allowed response when route path matches another HTTP verb.
   - `F17.5`, `B17.1`, `Scenario 4`: Path traversal sanitization in `try_serve_static` rejecting `../` and `..\` attempts.
3. **Milestone 5 (Auth & Middleware Hardening)**:
   - `F15.4`: Schema column alignment between `003_create_sessions_table.matrix` (`token`) and `Auth.matrix` query (`WHERE s.id = ?`).

When Milestones M3, M4, and M5 are implemented, the entire 192-test suite is anticipated to achieve 100% pass rate.

---

## 4. Test Infrastructure Design

- **Clean Isolation**: Every test sets up its own state and performs automated teardown. Temporary tables, inserted models, and generated session cookies are deleted after verification, leaving zero database residue (`.sqlite-wal`, `.sqlite-shm` or orphaned rows).
- **Native Runtime**: Zero external framework dependencies; operates directly with `spid3r run` using native Spid3r language features and built-ins.
- **Diagnostics**: Detailed reporting with millisecond timings and exact failure diagnostics.
