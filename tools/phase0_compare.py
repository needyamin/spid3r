#!/usr/bin/env python3
"""Phase 0 & selfhost toolchain regression comparison and floor gate.

Supports both ``spid3r.phase0/1`` (``baseline-<sha>.json``,
``reference.p1.json``, ``results.phase0.ci-advisory.json``) and
``spid3r.selfhost.bench/1`` (``benchmarks/selfhost/baseline.json``).

Regression definition (Phase 11 P-7):
    median compute_ms OR wall_ms on any gated / tracked workload
    > previous median * 1.05
    AND (current - previous) > 3 * pooled_MAD
    confirmed on TWO consecutive runs (via ``--current r1 r2`` or
    push-to-push ``--state <state.json>``).

Read-only unless ``--out``, ``--state``, or ``--markdown`` is passed.
Stdlib only.
"""

import argparse
import json
import math
import os
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


def extract_medians(doc: dict) -> dict:
    """Normalize medians from baseline/phase0/selfhost JSON schemas."""
    out = {}
    if "medians" in doc:
        for w, variants in doc.get("medians", {}).items():
            for v, d in variants.items():
                if isinstance(d, dict):
                    out[(w, v)] = d
        return out

    for w, variants in doc.get("workloads", {}).items():
        if not isinstance(variants, dict):
            continue
        for v, d in variants.items():
            if not isinstance(d, dict):
                continue
            passes = d.get("passes", [])
            if not passes:
                continue
            p0 = passes[0]
            comp = p0.get("compute") or {}
            wall = p0.get("wall") or comp
            out[(w, v)] = {
                "compute_ms": comp.get("median"),
                "wall_ms": wall.get("median"),
                "mad": comp.get("mad", wall.get("mad", 0.0)),
                "compute_mb_s": d.get("compute_mb_s"),
                "programs_per_s": d.get("programs_per_s"),
                "median_ms": d.get("median_ms"),
            }
    return out


def extract_status_frozen(doc: dict) -> tuple[bool, str]:
    if "verdict" in doc and isinstance(doc["verdict"], dict):
        return bool(doc["verdict"].get("frozen")), str(doc["verdict"].get("status", "UNKNOWN"))
    return bool(doc.get("frozen")), str(doc.get("status", "UNKNOWN"))


def check_selfhost_floors(floors_doc: dict, run_doc: dict) -> list[dict]:
    """Check selfhost workload throughput/latency against frozen Tier-A floors."""
    targets = floors_doc.get("targets") or {}
    workloads = run_doc.get("workloads") or {}
    out = []
    floor_specs = (
        ("lex", "tier_a_floor_mb_s", "compute_mb_s", ">=", "MB/s"),
        ("parse", "tier_a_floor_programs_s", "programs_per_s", ">=", "prog/s"),
        ("check", "tier_a_floor_programs_s", "programs_per_s", ">=", "prog/s"),
        ("self_compile", "tier_a_floor_mb_s", "compute_mb_s", ">=", "MB/s"),
        ("corpus_build", "tier_a_max_batch_ms", "median_ms", "<=", "ms"),
    )
    for w, floor_key, val_key, cmp_op, unit in floor_specs:
        t = targets.get(w)
        sh = (workloads.get(w) or {}).get("selfhost")
        if not t or floor_key not in t or not isinstance(sh, dict):
            continue
        floor = float(t[floor_key])
        val = sh.get(val_key)
        if val is None:
            continue
        val = float(val)
        passed = (val >= floor) if cmp_op == ">=" else (val <= floor)
        out.append({
            "workload": w,
            "role": "selfhost",
            "metric": val_key,
            "unit": unit,
            "op": cmp_op,
            "floor": floor,
            "measured": val,
            "pass": passed,
            "status": "FLOOR_PASS" if passed else "FLOOR_FAIL",
        })
    return out


def render_markdown(title: str, report: dict) -> str:
    lines = [f"### {title}", ""]
    lines.append(
        f"- **Verdict**: `{report['verdict']}` | **Rule**: {report['regression_rule']}"
    )
    mismatches = report.get("metadata", {}).get("mismatches") or []
    if mismatches:
        lines.append(f"- **Comparability notice**: {', '.join(mismatches)}")
    lines.append("")
    lines.append("| Workload | Variant | Metric | Prev (ms) | Cur (ms) | Delta | Ratio | 3×Pooled MAD | Status |")
    lines.append("| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | --- |")
    for c in report["checks"]:
        if c["status"] in ("NO_BASELINE", "MISSING"):
            continue
        if "runs" not in c:
            lines.append(f"| `{c['workload']}` | `{c['variant']}` | - | - | - | - | - | - | `{c['status']}` |")
            continue
        last = c["runs"][-1]
        prev = last.get("previous")
        cur = last.get("current")
        delta = last.get("delta_ms")
        ratio = last.get("ratio")
        pm3 = (3.0 * last["pooled_mad"]) if "pooled_mad" in last else None
        pct = f"{(ratio - 1.0) * 100.0:+.2f}%" if isinstance(ratio, (int, float)) else "-"
        p_s = f"{prev:.3f}" if isinstance(prev, (int, float)) else "-"
        c_s = f"{cur:.3f}" if isinstance(cur, (int, float)) else "-"
        r_s = f"{ratio:.4f}×" if isinstance(ratio, (int, float)) else "-"
        m_s = f"{pm3:.3f} ms" if isinstance(pm3, (int, float)) else "-"
        lines.append(
            f"| `{c['workload']}` | `{c['variant']}` | `{c['metric']}` | {p_s} | {c_s} | {pct} | {r_s} | {m_s} | **{c['status']}** |"
        )

    floors = report.get("floor_checks") or []
    if floors:
        lines.extend(["", "#### Selfhost Tier-A Floors", ""])
        lines.append("| Workload | Metric | Floor | Measured | Status |")
        lines.append("| --- | --- | ---: | ---: | --- |")
        for fc in floors:
            lines.append(
                f"| `{fc['workload']}` | `{fc['metric']}` | `{fc['op']} {fc['floor']} {fc['unit']}` | `{fc['measured']} {fc['unit']}` | **{fc['status']}** |"
            )
    lines.append("")
    return "\n".join(lines)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--baseline", required=True, help="baseline/previous phase0 or selfhost JSON")
    ap.add_argument("--current", nargs="+", required=True,
                    help="one or two current measurement JSONs (two = consecutive confirmation)")
    ap.add_argument("--floors", help="optional selfhost baseline JSON containing Tier-A target floors")
    ap.add_argument("--state", help="optional JSON state file tracking cross-push consecutive regression streaks")
    ap.add_argument("--out", help="optional path to write the comparison report JSON")
    ap.add_argument("--markdown", help="optional path to write Markdown delta summary")
    ap.add_argument("--title", default="Performance Tracking (Phase 11 P-7)",
                    help="heading for Markdown summary")
    ap.add_argument("--github-summary", action="store_true",
                    help="append Markdown summary to $GITHUB_STEP_SUMMARY when set")
    ap.add_argument("--advisory", action="store_true",
                    help="report deltas/warnings cleanly without failing exit code")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args(argv)

    base = load(args.baseline)
    runs = [load(p) for p in args.current]
    prev = extract_medians(base)
    base_frozen, base_status = extract_status_frozen(base)

    state_data = {}
    if args.state and Path(args.state).is_file():
        state_data = load(args.state)
    prev_streaks = state_data.get("streaks") or {}
    new_streaks: dict[str, int] = {}

    report = {
        "schema": "spid3r.phase0.compare/1",
        "baseline": args.baseline,
        "current_runs": args.current,
        "baseline_frozen": base_frozen,
        "baseline_status": base_status,
        "regression_rule": ">1.05x previous median AND >3x pooled MAD, confirmed on 2 consecutive runs",
        "pooled_mad_method": "sqrt((mad_prev^2 + mad_cur^2)/2)",
        "checks": [],
        "floor_checks": [],
        "floor_failures": [],
        "metadata": {},
        "confirmed_regressions": [],
    }

    if base_frozen is not True:
        report["metadata"]["baseline_not_frozen"] = (
            "Baseline is ADVISORY_ONLY (unpinned/shared host); comparison is informational unless strict."
        )

    # Metadata comparability checks.
    for label, getter in (
        ("checksums", lambda d: d.get("checksums")),
        ("source_sha256", lambda d: d.get("source_sha256")),
        ("cpu", lambda d: d.get("host", {}).get("cpu")),
        ("os", lambda d: d.get("host", {}).get("os")),
    ):
        b = getter(base)
        if b is None and label in ("checksums", "source_sha256"):
            continue
        for i, run in enumerate(runs):
            c = getter(run)
            if c is not None and b is not None and b != c:
                report["metadata"].setdefault("mismatches", []).append(
                    f"run{i}:{label} differs from baseline")

    host_mismatch = any(
        (":cpu " in m or ":os " in m) for m in report["metadata"].get("mismatches", [])
    )

    cur_medians = [extract_medians(r) for r in runs]
    keys = sorted(set(prev) | set().union(*[set(cm) for cm in cur_medians]))
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
            for i, cm in enumerate(cur_medians):
                c = cm.get((w, v))
                if c is None or c.get(metric) is None:
                    per_run.append({"run": i, "status": "MISSING"})
                    continue
                cval = c[metric]
                pm = pooled_mad(p.get("mad", 0.0), c.get("mad", 0.0))
                ratio = cval / pval if pval else float("inf")
                delta = cval - pval
                flagged = bool(ratio > 1.05 and delta > 3.0 * pm)
                per_run.append({
                    "run": i,
                    "previous": round(pval, 4),
                    "current": round(cval, 4),
                    "ratio": round(ratio, 6),
                    "delta_ms": round(delta, 4),
                    "pooled_mad": round(pm, 4),
                    "flagged": flagged,
                })

            state_key = f"{w}/{v}/{metric}"
            streak = int(prev_streaks.get(state_key, 0))
            for r in per_run:
                if r.get("status") == "MISSING":
                    continue
                streak = (streak + 1) if r.get("flagged") else 0
            new_streaks[state_key] = streak

            flagged_runs = [r for r in per_run if r.get("flagged")]
            present_runs = [r for r in per_run if r.get("status") != "MISSING"]
            in_invocation_confirmed = len(flagged_runs) >= 2 and len(flagged_runs) == len(runs)
            cross_push_confirmed = bool(args.state and streak >= 2)
            confirmed = bool((in_invocation_confirmed or cross_push_confirmed) and not host_mismatch)
            status = (
                "MISSING"
                if not present_runs
                else (
                    "REGRESSION"
                    if confirmed
                    else ("INFO_MISMATCH" if host_mismatch and flagged_runs else ("WATCH" if flagged_runs else "OK"))
                )
            )
            entry = {
                "workload": w,
                "variant": v,
                "metric": metric,
                "runs": per_run,
                "consecutive_flags": streak,
                "confirmed": confirmed,
                "status": status,
            }
            report["checks"].append(entry)
            if confirmed:
                report["confirmed_regressions"].append(entry)

    if args.floors:
        floors_doc = load(args.floors)
        floor_checks = check_selfhost_floors(floors_doc, runs[-1])
        report["floor_checks"] = floor_checks
        report["floor_failures"] = [fc for fc in floor_checks if not fc["pass"]]

    has_failure = bool(report["confirmed_regressions"] or report["floor_failures"])
    if has_failure:
        report["verdict"] = "FAIL"
    elif host_mismatch or base_frozen is not True:
        report["verdict"] = "ADVISORY"
    else:
        report["verdict"] = "PASS"
    report["pass"] = report["verdict"] != "FAIL"

    text = json.dumps(report, indent=2) + "\n"
    if args.out:
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        Path(args.out).write_text(text, encoding="utf-8")
    if args.state:
        sp = Path(args.state)
        sp.parent.mkdir(parents=True, exist_ok=True)
        sp.write_text(json.dumps({"streaks": new_streaks}, indent=2) + "\n", encoding="utf-8")

    md = render_markdown(args.title, report)
    if args.markdown:
        mp = Path(args.markdown)
        mp.parent.mkdir(parents=True, exist_ok=True)
        mp.write_text(md, encoding="utf-8")
    if args.github_summary:
        summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
        if summary_path:
            with open(summary_path, "a", encoding="utf-8") as fh:
                fh.write("\n" + md)

    if not args.quiet:
        for c in report["checks"]:
            if "runs" not in c:
                print(f"{c['workload']:<12} {c['variant']:<8} {c['status']}")
                continue
            r0 = c["runs"][-1]
            cur = r0.get("current", "-")
            ratio = r0.get("ratio", "-")
            print(f"{c['workload']:<12} {c['variant']:<8} {c['metric']:<10} "
                  f"prev {r0.get('previous', '-'):>10} cur {cur:>10} "
                  f"ratio {ratio} {c['status']}")
        for fc in report["floor_checks"]:
            print(f"FLOOR {fc['workload']:<12} {fc['metric']:<15} {fc['measured']} "
                  f"({fc['op']} {fc['floor']} {fc['unit']}) {fc['status']}")
        print(f"PHASE0 COMPARE: {report['verdict']} "
              f"({len(report['confirmed_regressions'])} confirmed regression(s), "
              f"{len(report['floor_failures'])} floor failure(s))")

    if has_failure and args.advisory:
        if os.environ.get("GITHUB_ACTIONS") == "true":
            print(
                f"::warning::Advisory perf check flagged {len(report['confirmed_regressions'])} "
                f"regression(s) and {len(report['floor_failures'])} floor miss(es)"
            )
        return 0
    return 0 if report["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
