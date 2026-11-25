# Selfhost Toolchain Throughput Baselines (Phase 11 `P-2`)

Frozen Stage-0 (Rust) vs selfhost (`spid3r-self` compiled via `stage0 build --release`) compiler throughput baselines under the Phase 0 measurement protocol (`tools/phase0_bench.py --selfhost-bench`).

## Protocol & Fairness

- **Harness**: `python tools/phase0_bench.py --selfhost-bench --pin-core 6 --out benchmarks/selfhost/baseline.json`
- **Samples**: 9 warmups, 21 measured repeats, 2 passes; `MAD/median <= 5%`, two-pass median agreement `<= 3%`, empty-workload startup MAD `<= 2.0 ms`. Noisy `INVALID_RERUN` attempts are discarded and retried (up to 3 attempts) without loosening thresholds.
- **Blocking sensitivity gate**: 5% `phase0_selftest` (`map.matrix` B/P/C arms) passed before measurement (`selftest_pass: True`).
- **Host / build**: `Windows-11-10.0.26300-SP0`, `Intel(R) Core(TM) Ultra 7 265K` (pinned P-core `6`), `spid3r 1.1.0` (`cef1a36`, Stage 1 `stage0 build --release`). Status: `ADVISORY_ONLY` (shared desktop host; ratios are portable across machines, raw ms are host-specific).

## Frozen Workloads (`MANIFEST.sha256`)

| File | Scope | Size | SHA-256 |
| --- | --- | ---: | --- |
| `lex_workload.matrix` | 16x concatenated positive corpus | 1,016,880 B | `5bf8459cbced9ec13dc91f75e1dacc8c989c37a6612d77d29578b6d769800f80` |
| `parse_check_list.txt` | 120 positive corpus programs | 6,075 B | `1d002f2e10def0536b9f9b3ecd3ea46929d156e65c87246c4c13d043a0fcacb4` |
| `selfcompile_modules.txt` | 17 transitive `selfhost/` modules (`main.matrix`) | 392 B (584,703 B src) | `fe0b6844ff22e9422c85af9342ecbdf7c369148f5b065ed98b8ebf4994019717` |
| `corpus_build_list.txt` | 6 standalone examples (`class`, `functions`, `hello`, `higher_order`, `loops`, `map`) | 237 B | `c2b62b25d7c6356a994c6c77e9bec70b55776ad2282dab9fe1660ec9211ca4d` |

Verify integrity fail-closed before measuring:

```bash
python tools/phase0_bench.py --verify-selfhost
```

## Stage-0 vs Selfhost Baseline Table (`benchmarks/selfhost/baseline.json`)

| Stage | Workload | Stage 0 (Rust) | Selfhost (AOT) | Selfhost vs Stage 0 | MAD / 2-pass (S0 / SH) | Tier-A Floor (-20%) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `lex` | `lex_workload.matrix` (1.02 MB) | `16.5506 MB/s` (`61.44 ms`) | `0.2397 MB/s` (`4241.82 ms`) | `0.0145x` (`69.0x` slower) | `0.80% / 0.75%` · `0.59% / 0.13%` | `>= 0.192 MB/s` |
| `parse` | `parse_check_list.txt` (121 files) | `87.364 prog/s` (`1385.00 ms`) | `71.959 prog/s` (`1681.52 ms`) | `0.8237x` | `0.78% / 0.60%` · `2.44% / 1.70%` | `>= 57.6 prog/s` |
| `check` | `parse_check_list.txt` (121 files) | `91.819 prog/s` (`1317.81 ms`) | `53.965 prog/s` (`2242.21 ms`) | `0.5877x` | `0.92% / 0.35%` · `2.52% / 2.12%` | `>= 43.2 prog/s` |
| `self_compile` | `selfcompile_modules.txt` (17 mods, 585 KB) | `0.45667 MB/s` (`1280.35 ms`) | `0.04823 MB/s` (`12123.26 ms`) | `0.1056x` (`9.47x` slower) | `0.99% / 0.61%` · `0.61% / 0.50%` | `>= 0.0386 MB/s` |
| `corpus_build` | `corpus_build_list.txt` (6 examples) | `975.66 ms` (`6.15 prog/s`) | `2205.89 ms` (`2.72 prog/s`) | `2.2609x` slower | `1.70% / 0.05%` · `1.24% / 1.25%` | `<= 2757.4 ms` |

## Throughput Gate Consumers (`P-1..P-7`)

The Stage 1–3 gates read both the pinned workload paths and their Tier-A floors directly from `benchmarks/selfhost/baseline.json` (verifying `MANIFEST.sha256` on startup):

- `tools/lex_bench.py` — reads `targets.lex.tier_a_floor_mb_s` (`0.192 MB/s`) and `lex_workload.matrix`.
- `tools/parse_bench.py` — reads `targets.parse.tier_a_floor_programs_s` (`57.6 prog/s`) and `parse_check_list.txt`.
- `tools/check_bench.py` — reads `targets.check.tier_a_floor_programs_s` (`43.2 prog/s`) and `parse_check_list.txt`.
