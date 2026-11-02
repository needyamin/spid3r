# E2E Test Infra: Spid3rWeb

## Test Philosophy
- Opaque-box, requirement-driven. No dependency on internal implementation design.
- Methodology: Category-Partition + Boundary Value Analysis + Pairwise Combinatorial + Real-World Workload Testing.

## Feature Inventory
| # | Feature | Source (requirement) | Tier 1 | Tier 2 | Tier 3 |
|---|---------|---------------------|:------:|:------:|:------:|
| 1 | Magic CLI Test Subcommand | ORIGINAL_REQUEST §R1, R3 | 5 | 5 | ✓ |
| 2 | Magic CLI Help & Route Listing | ORIGINAL_REQUEST §R1, R3 | 5 | 5 | ✓ |
| 3 | Package Manifest Toolchain | ORIGINAL_REQUEST §R2 | 5 | 5 | ✓ |
| 4 | Active Record ORM CRUD & Queries | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ |
| 5 | Active Record Relationships | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ |
| 6 | Database Migrations & DDL | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ |
| 7 | Router Pattern & Param Extraction | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ |
| 8 | Router Method Dispatch & 405 Status | ORIGINAL_REQUEST §R1, R2 | 5 | 5 | ✓ |
| 9 | Request Validation Engine | ORIGINAL_REQUEST §R1, R2 | 5 | 5 | ✓ |
| 10 | Request Parsing & Binary Body | ORIGINAL_REQUEST §R1, R2 | 5 | 5 | ✓ |
| 11 | Response & Cookie Formatting | ORIGINAL_REQUEST §R1, R2 | 5 | 5 | ✓ |
| 12 | Global & Route Middleware Pipeline | ORIGINAL_REQUEST §R1, R2 | 5 | 5 | ✓ |
| 13 | SpiderBD Variable Interpolation (Dot-Notation) | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ |
| 14 | SpiderBD Directives & Layout Inheritance | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ |
| 15 | Authentication & CSPRNG Session Lifecycle | ORIGINAL_REQUEST §R1, R2 | 5 | 5 | ✓ |
| 16 | Password Hashing (Bcrypt) & Verification | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ |
| 17 | Static Asset Delivery & Security Traversal Defense | ORIGINAL_REQUEST §R1, R2 | 5 | 5 | ✓ |

## Real-World Application Scenarios (Tier 4)
| # | Scenario | Features Exercised | Complexity |
|---|----------|--------------------|------------|
| 1 | Complete User Registration & Login Session Flow | F7, F9, F11, F12, F13, F14, F15, F16 | High |
| 2 | Article Publishing, Editing & Dot-Notation Detail Rendering | F4, F5, F7, F9, F11, F12, F13, F14, F15 | High |
| 3 | REST API CRUD with JSON Request/Response & Validation Errors | F7, F8, F9, F10, F11, F12 | Medium |
| 4 | Security Defenses (Tampered Cookies, Directory Traversal, 405 Rejection) | F8, F11, F12, F15, F17 | High |
| 5 | CLI Workflow Lifecycle (`migrate`, `db:seed`, `route:list`, `test`) | F1, F2, F3, F6 | Medium |

## Coverage Thresholds
- Tier 1: ≥5 per feature (≥85 test cases across 17 features)
- Tier 2: ≥5 per feature for boundaries & error handling (≥85 test cases)
- Tier 3: Pairwise combinations of major feature interactions (≥17 test cases)
- Tier 4: ≥5 realistic end-to-end application scenarios
- Tier 5: Adversarial coverage hardening (white-box gap analysis, executed in M6)
