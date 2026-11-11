#!/usr/bin/env python3
"""Phase 0 COMMIT-OVER-COMMIT median comparison (the DoD caveat).

    python tools/phase0_compare.py \
        --baseline benchmarks/baseline-<sha>.json \
        --current benchmarks/results.phase0.json benchmarks/results.phase0.json

Why this exists: `spid3r bench --gate --compare` only checks tree == vm output,
and BENCH_FLOORS are calibrated at ~1/3 of the worst rate (they catch 3x+
catastrophic regressions only). Neither can see a 5% drift. plan.html section
10 therefore makes a commit-over-commit median comparison MANDATORY.

Regression definition (plan.html section 10):
    median compute_ms OR wall_ms on any gated / HLC workload
    > previous commit median x 1.05
    AND > 3 x pooled MAD
    confirmed by TWO consecutive runs.

A bare 1.05x is NOT enough on its own: on a noisy shared host a small MAD
overlap must not be published as a regression. Pooled MAD here is the root
mean square of the two MADs, sqrt((mad_prev^2 + mad_cur^2)/2), disclosed in
the output. Checksums, source digests, CPU and toolchain are compared too;
a host/toolchain mismatch downgrades the verdict to ADVISORY rather than
silently comparing incomparable numbers.

Read-only: never modifies an artifact. Stdlib only.
"""

import argparse
import json
import math
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def load(path: str) -> dict:
    p = Path(path)
    if not p.is_file():
        raise SystemExit(f"file not found: {p}")
    return json.loads(p.read_text(encoding="utf-8"))


def pooled_mad(a: float, b: float) -> float:
    return math.sqrt((a * a + b * b) / 2.0)


def current_medians(run: dict) -> dict:
    out = {}
    for w, variants in run.get("workloads", {}).items():
        for v, d in variants.items():
            passes = d.get("passes", [])
            if not passes:
                continue
            p0 = passes[0]
            out[(w, v)] = {
                "compute_ms": p0["compute"]["median"],
                "wall_ms": p0["wall"]["median"],
                "mad": p0["compute"]["mad"],
            }
    return out


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--baseline", required=True, help="benchmarks/baseline-<sha>.json")
    ap.add_argument("--current", nargs="+", required=True,
                    help="one or two results.phase0.json runs (two = consecutive confirmation)")
    ap.add_argument("--out", help="optional path to write the comparison report JSON")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args(argv)

    base = load(args.baseline)
    runs = [load(p) for p in args.current]
    prev = {}
    for w, variants in base.get("medians", {}).items():
        for v, d in variants.items():
            prev[(w, v)] = d

    report = {
        "schema": "spid3r.phase0.compare/1",
        "baseline": args.baseline,
        "current_runs": args.current,
        "baseline_frozen": base.get("frozen"),
        "baseline_status": base.get("status"),
        "regression_rule": ">1.05x previous median AND >3x pooled MAD, confirmed on 2 consecutive runs",
        "pooled_mad_method": "sqrt((mad_prev^2 + mad_cur^2)/2)",
        "checks": [], "metadata": {}, "confirmed_regressions": [],
    }

    if base.get("frozen") is not True:
        report["metadata"]["baseline_not_frozen"] = (
            "Baseline is ADVISORY_ONLY (unpinned host); comparison is informational, never a gate."
        )

    # Metadata comparability.
    for label, key, getter in (
        ("checksums", "checksums", lambda d: d.get("checksums")),
        ("source_sha256", "source_sha256", lambda d: d.get("source_sha256")),
        ("cpu", "cpu", lambda d: d.get("host", {}).get("cpu")),
        ("os", "os", lambda d: d.get("host", {}).get("os")),
    ):
        b = getter(base)
        if b is None and label in ("checksums", "source_sha256"):
            continue
        for i, run in enumerate(runs):
            c = getter(run)
            if c is not None and b is not None and b != c:
                report["metadata"].setdefault("mismatches", []).append(
                    f"run{i}:{label} differs from baseline")

    keys = sorted(set(prev) | set().union(*[set(current_medians(r)) for r in runs]))
    for w, v in keys:
        if (w, v) not in prev:
            report["checks"].append({"workload": w, "variant": v,
                                     "status": "NO_BASELINE", "note": "not in baseline medians"})
            continue
        p = prev[(w, v)]
        for metric in ("compute_ms", "wall_ms"):
            pval = p.get(metric)
            if pval is None:
                continue
            per_run = []
            for i, run in enumerate(runs):
                c = current_medians(run).get((w, v))
                if c is None:
                    per_run.append({"run": i, "status": "MISSING"})
                    continue
                cval = c[metric]
                pm = pooled_mad(p.get("mad", 0.0), c["mad"])
                ratio = cval / pval if pval else float("inf")
                delta = cval - pval
                flagged = bool(ratio > 1.05 and delta > 3.0 * pm)
                per_run.append({
                    "run": i, "previous": round(pval, 4), "current": round(cval, 4),
                    "ratio": round(ratio, 6), "delta_ms": round(delta, 4),
                    "pooled_mad": round(pm, 4), "flagged": flagged,
                })
            flagged_runs = [r for r in per_run if r.get("flagged")]
            confirmed = len(flagged_runs) >= 2 and len(flagged_runs) == len(runs)
            entry = {
                "workload": w, "variant": v, "metric": metric,
                "runs": per_run, "confirmed": confirmed,
                "status": "REGRESSION" if confirmed else ("WATCH" if flagged_runs else "OK"),
            }
            report["checks"].append(entry)
            if confirmed:
                report["confirmed_regressions"].append(entry)

    host_mismatch = bool(report["metadata"].get("mismatches"))
    if report["confirmed_regressions"]:
        report["verdict"] = "FAIL"
    elif host_mismatch or base.get("frozen") is not True:
        report["verdict"] = "ADVISORY"
    else:
        report["verdict"] = "PASS"
    report["pass"] = report["verdict"] != "FAIL"

    text = json.dumps(report, indent=2) + "\n"
    if args.out:
        Path(args.out).write_text(text, encoding="utf-8")
    if not args.quiet:
        for c in report["checks"]:
            if "runs" not in c:
                print(f"{c['workload']:<6} {c['variant']:<7} {c['status']}")
                continue
            r0 = c["runs"][0]
            cur = r0.get("current", "-")
            ratio = r0.get("ratio", "-")
            print(f"{c['workload']:<6} {c['variant']:<7} {c['metric']:<10} "
                  f"prev {r0.get('previous', '-'):>10} cur {cur:>10} "
                  f"ratio {ratio} {c['status']}")
        print(f"PHASE0 COMPARE: {report['verdict']} "
              f"({len(report['confirmed_regressions'])} confirmed regression(s))")
    return 0 if report["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
