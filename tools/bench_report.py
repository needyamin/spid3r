#!/usr/bin/env python3
"""Measure the Spid3r benchmark suite and render docs/html/benchmark.html.

Two phases, so the page can be re-rendered without re-measuring:

    python tools/bench_report.py --measure   # writes benchmarks/report-data.json
    python tools/bench_report.py --render    # writes docs/html/benchmark.html
    python tools/bench_report.py             # both

Everything here is wall-clock, best-of-N, measured by spawning a fresh process
per run - the same method `spid3r bench` itself uses, and the same method the
historical numbers in benchmarks/results.json were taken with. Interpreter
startup is measured separately and reported as its own column, because for the
fastest benchmarks it is a large share of the total (PHP takes ~30 ms just to
start, which is more than the whole `json` workload).
"""

import argparse
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BENCH = ROOT / "benchmarks"
OUT_HTML = ROOT / "docs" / "html" / "benchmark.html"
OUT_JSON = BENCH / "report-data.json"

# Outer-iteration counts, mirrored from BENCH_ITERS in src/cli.rs so the report
# can turn a wall time into iters/sec without re-deriving it from the table.
ITERS = {
    "arith": 2_000_000,
    "strings": 30_000,
    "list": 200_000,
    "map": 40_000,
    "methods": 200_000,
    "closures": 40_000,
    "json": 2_000,
    "tasks": 2_000,
}

# Perf-gate floors, mirrored from BENCH_FLOORS in src/cli.rs.
FLOORS = {
    "arith": 6_000_000,
    "strings": 140_000,
    "list": 800_000,
    "map": 160_000,
    "methods": 500_000,
    "closures": 170_000,
    "json": 8_000,
    "tasks": 6_000,
}

# Display order, mirroring the table in the plan (optimize_plan.html section 2).
# report-data.json keeps whatever order `spid3r bench` emits; only the printed
# summary and the ratio column follow this one.
WORKLOAD_ORDER = ["arith", "methods", "strings", "list", "map", "closures", "json", "tasks"]

BENCH_NAMES = {
    "arith": "Arithmetic &amp; Loop",
    "strings": "String Building",
    "list": "List Churn",
    "map": "Map / Dict",
    "methods": "Method Dispatch",
    "closures": "Closures",
    "json": "JSON Encode / Decode",
    "tasks": "Spawn &amp; Join",
    "stress_suite": "Stress Suite",
}

BENCH_DESC = {
    "arith": "2M-iteration integer accumulation loop",
    "strings": "30K string concatenations and slices",
    "list": "200K pushes then pops, summing values",
    "map": "40K hash inserts and lookups",
    "methods": "200K method calls on objects",
    "closures": "40K closure allocations and calls",
    "json": "2K encode/decode round-trips",
    "tasks": "2K spawn/join task round-trips",
    "stress_suite": "Mixed workload, correctness only (ungated)",
}


def bin_path() -> Path:
    for name in ("spid3r.exe", "spid3r"):
        p = ROOT / "target" / "release" / name
        if p.exists():
            return p
    sys.exit("target/release/spid3r not found - run `cargo build --release` first")


TIME_RE = re.compile(r"^(?P<stem>[a-z_]+)\s+(?P<iters>\d+|-)\s+(?P<time>[\d.]+)(?P<unit>ms|s|µs)\s+(?P<rate>[\d.]+|-)\s*$")


def parse_bench_table(out: str):
    rows = {}
    for line in out.splitlines():
        m = TIME_RE.match(line.strip())
        if not m:
            continue
        unit = m.group("unit")
        val = float(m.group("time"))
        ms = val * 1000 if unit == "s" else (val / 1000 if unit == "µs" else val)
        rows[m.group("stem")] = {"ms": ms}
    return rows


def run_bench(engine: str, runs: int, compare: bool = False):
    cmd = [str(bin_path()), "bench", "--engine", engine, "--runs", str(runs)]
    if compare:
        # `--compare` makes the harness require byte-identical output from the
        # other engine, so the timing run doubles as the differential check the
        # report claims. A divergence exits non-zero and we stop here.
        cmd.append("--compare")
    proc = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True)
    if proc.returncode != 0:
        sys.exit(f"`spid3r bench --engine {engine}` failed:\n{proc.stdout}\n{proc.stderr}")
    return parse_bench_table(proc.stdout), proc.stdout


def best_of(cmd, runs):
    """Best wall-clock of N fresh process runs, in ms."""
    best = float("inf")
    for _ in range(runs):
        t0 = time.perf_counter()
        subprocess.run(cmd, cwd=ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        best = min(best, (time.perf_counter() - t0) * 1000)
    return best


def startup_ms(cmd, runs=9):
    return best_of(cmd, runs)


# A startup-adjusted number is only a measurement if the adjusted time is
# larger than the noise in the startup estimate it was derived from. That
# noise is the median absolute deviation of the startup samples; 0.5 ms is the
# absolute floor below which a process-spawn measurement on a shared desktop
# means nothing at all. Rows whose reference lands under its own resolution
# are reported as unresolved rather than divided into a fake ratio -- see the
# docstring of ratio_for().
RESOLUTION_ABS_MS = 0.5
RESOLUTION_MAD_FACTOR = 2.0


def startup_stats(cmd, runs=9):
    """(best_ms, mad_ms) for a no-op invocation.

    `best_ms` is the fixed cost subtracted from a workload; `mad_ms` is how
    much that estimate wobbles between runs, which is the resolution limit of
    everything derived from it.
    """
    samples = []
    for _ in range(runs):
        t0 = time.perf_counter()
        subprocess.run(cmd, cwd=ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        samples.append((time.perf_counter() - t0) * 1000)
    samples.sort()
    best = samples[0]
    med = samples[len(samples) // 2]
    mad = sorted(abs(s - med) for s in samples)[len(samples) // 2]
    return best, mad


def measure(runs_spid3r=5, runs_tree=3, runs_other=3):
    binp = str(bin_path())
    data = {
        "generated": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        "host": {
            "os": platform.platform(),
            "cpu": cpu_name(),
            "cores": os.cpu_count(),
            "python": platform.python_version(),
        },
        "runs": {"spid3r": runs_spid3r, "tree": runs_tree, "other": runs_other},
        "spid3r_version": subprocess.run([binp, "--version"], capture_output=True, text=True)
        .stdout.strip()
        .splitlines()[0],
        "startup": {},
        "vm": {},
        "tree": {},
        "php": {},
        "python": {},
        "gate": [],
    }

    data["startup"] = {}
    data["startup_mad"] = {}
    data["resolution"] = {}

    def record_startup(key, cmd):
        best, mad = startup_stats(cmd)
        data["startup"][key] = best
        data["startup_mad"][key] = mad
        data["resolution"][key] = max(RESOLUTION_ABS_MS, RESOLUTION_MAD_FACTOR * mad)

    record_startup("spid3r", [binp, "--version"])
    if shutil.which("php"):
        record_startup("php", ["php", "-r", ""])
    if shutil.which("python"):
        record_startup("python", [sys.executable, "-c", ""])
    if shutil.which("php"):
        data["php_version"] = (
            subprocess.run(["php", "-r", "echo PHP_VERSION;"], capture_output=True, text=True).stdout.strip()
        )

    vm_rows, vm_out = run_bench("vm", runs_spid3r, compare=True)
    data["compare"] = True
    data["vm_raw"] = vm_out
    tree_rows, tree_out = run_bench("tree", runs_tree)
    data["tree_raw"] = tree_out

    for stem, row in vm_rows.items():
        data["vm"][stem] = row["ms"]
    for stem, row in tree_rows.items():
        data["tree"][stem] = row["ms"]

    # Reference implementations: benchmarks/php/*.php and benchmarks/python/*.py
    # (json lives in json_bench.*).
    for stem in vm_rows:
        fname = "json_bench" if stem == "json" else stem
        php = BENCH / "php" / f"{fname}.php"
        if php.exists() and shutil.which("php"):
            data["php"][stem] = best_of(["php", str(php)], runs_other)
        py = BENCH / "python" / f"{fname}.py"
        if py.exists():
            data["python"][stem] = best_of([sys.executable, str(py)], runs_other)

    for stem in vm_rows:
        if stem not in ITERS:
            continue
        ms = data["vm"][stem]
        rate = ITERS[stem] / (ms / 1000.0)
        floor = FLOORS.get(stem)
        data["gate"].append(
            {
                "stem": stem,
                "rate": rate,
                "floor": floor,
                "margin": (rate / floor) if floor else None,
                "pass": (rate >= floor) if floor else None,
            }
        )

    # Startup-adjusted numbers, and the ratio the whole speed program is
    # written in. Absolute milliseconds are host-specific (this is a shared,
    # unpinned machine), so the portable claim is:
    #
    #     ratio = (spid3r - spid3r_startup) / min(php - php_startup,
    #                                              python - python_startup)
    #
    # Both engines share one binary, so both subtract the same startup. These
    # are NEW keys: the existing `vm` / `tree` / `php` / `python` maps keep
    # their raw, unadjusted meaning because docs/html/benchmark.html renders
    # them and the perf floors are computed from them.
    data["adj"] = {}
    for engine, startup_key in (
        ("vm", "spid3r"),
        ("tree", "spid3r"),
        ("php", "php"),
        ("python", "python"),
    ):
        startup = data["startup"].get(startup_key, 0.0) or 0.0
        data["adj"][engine] = {
            stem: max(0.0, ms - startup) for stem, ms in data[engine].items()
        }

    # Two ratios, because they answer two different questions.
    #
    #   ratio_php  -- spid3r / PHP, always computable here, and STABLE: PHP's
    #                 startup is ~25 ms with ~1 ms jitter, so its 2-22 ms
    #                 workloads sit well clear of the noise. This is the gate.
    #   ratio_best -- spid3r / min(resolved php, python), the figure the plan's
    #                 section 2 quotes. CPython on this host is the bundled
    #                 3.13 with a ~197 ms startup; its 1-15 ms workloads are
    #                 inside its own jitter, so whether it "wins" a row flips
    #                 between runs. It is reported, not gated on.
    #
    # A reference contributes to ratio_best only if its adjusted time clears
    # its own resolution. Dividing by anything smaller is arithmetic on noise:
    # CPython's adjusted time for `json` comes out at 0.0 ms in some sessions.
    data["ratio_php"] = {}
    data["ratio_adj"] = {}
    data["ratio_ref"] = {}
    data["ratio_resolved"] = {}
    for engine in ("vm", "tree"):
        vs_php, vs_best, refs_used, resolved = {}, {}, {}, {}
        for stem, adj in data["adj"][engine].items():
            php_adj = data["adj"]["php"].get(stem)
            if php_adj is not None and php_adj > data["resolution"].get("php", RESOLUTION_ABS_MS):
                vs_php[stem] = adj / php_adj

            usable = {
                ref: data["adj"][ref][stem]
                for ref in ("php", "python")
                if stem in data["adj"][ref]
                and data["adj"][ref][stem] > data["resolution"].get(ref, RESOLUTION_ABS_MS)
            }
            resolved[stem] = bool(usable)
            if usable:
                best_ref = min(usable, key=usable.get)
                vs_best[stem] = adj / usable[best_ref]
                refs_used[stem] = best_ref
        data["ratio_php"][engine] = vs_php
        data["ratio_adj"][engine] = vs_best
        data["ratio_ref"][engine] = refs_used
        data["ratio_resolved"][engine] = resolved

    return data


def ordered_workloads(data):
    """The plan's display order, then anything else the harness reported."""
    known = [s for s in WORKLOAD_ORDER if s in data["vm"]]
    extra = [s for s in data["vm"] if s not in WORKLOAD_ORDER]
    return known + extra


def print_ratios(data):
    """One row per workload: adjusted ms per engine, the stable ratio against
    PHP, and the plan's "best reference" ratio where it is resolvable."""
    adj = data.get("adj") or {}
    vs_php = data.get("ratio_php") or {}
    vs_best = data.get("ratio_adj") or {}
    refs_used = data.get("ratio_ref") or {}
    resolved = data.get("ratio_resolved") or {}

    def fmt(v, width, prec=1):
        return f"{'-':>{width}}" if v is None else f"{v:>{width}.{prec}f}"

    print()
    print(
        f"{'workload':<10}{'vm adj':>9}{'vm/php':>8}{'vm/best':>9}"
        f"{'tree adj':>10}{'tree/php':>10}{'tree/best':>11}{'php adj':>9}{'py adj':>8}"
    )
    unresolved = []
    for stem in ordered_workloads(data):
        vm_adj = (adj.get("vm") or {}).get(stem)
        tree_adj = (adj.get("tree") or {}).get(stem)
        php_adj = (adj.get("php") or {}).get(stem)
        py_adj = (adj.get("python") or {}).get(stem)
        is_res = (resolved.get("vm") or {}).get(stem, False)
        if not is_res and stem in (adj.get("vm") or {}) and stem != "stress_suite":
            unresolved.append(stem)
        best_v = (vs_best.get("vm") or {}).get(stem) if is_res else None
        best_t = (vs_best.get("tree") or {}).get(stem) if is_res else None
        used = (refs_used.get("vm") or {}).get(stem, "")
        print(
            f"{stem:<10}{fmt(vm_adj, 9)}{fmt((vs_php.get('vm') or {}).get(stem), 8, 2)}"
            f"{fmt(best_v, 9, 2)}"
            f"{fmt(tree_adj, 10)}{fmt((vs_php.get('tree') or {}).get(stem), 10, 2)}"
            f"{fmt(best_t, 11, 2)}"
            f"{fmt(php_adj, 9)}{fmt(py_adj, 8)}   {used}"
        )
    print(
        "(ms, startup-adjusted; vm/php is the stable gate ratio; "
        "vm/best is spid3r / fastest reference -- lower is better)"
    )
    if unresolved:
        print(
            "best-reference n/a: the reference's compute is inside its own startup "
            f"resolution ({', '.join(unresolved)}). startup: "
            + ", ".join(f"{k} {v:.1f}ms" for k, v in (data.get("startup") or {}).items())
        )


def cpu_name():
    if platform.system() == "Windows":
        try:
            out = subprocess.run(
                ["powershell", "-NoProfile", "-Command", "(Get-CimInstance Win32_Processor).Name"],
                capture_output=True,
                text=True,
            ).stdout.strip()
            if out:
                return out.splitlines()[0]
        except OSError:
            pass
    if platform.system() == "Darwin":
        try:
            return subprocess.run(
                ["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True
            ).stdout.strip()
        except OSError:
            pass
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    except OSError:
        pass
    return platform.processor() or "unknown CPU"


def main():
    # Phase 0: `--mode compute` is a JSON-ONLY measurement entry point. It
    # delegates to tools/phase0_bench.py and NEVER renders docs/html/benchmark.html
    # and NEVER touches benchmarks/report-data.json. All other flags are passed
    # through unchanged, so `--mode compute --measure` runs the external
    # compute protocol; the default `report` mode is completely unaffected.
    argv = sys.argv[1:]
    if "--mode" in argv:
        i = argv.index("--mode")
        if i + 1 < len(argv) and argv[i + 1] == "compute":
            rest = argv[:i] + argv[i + 2:]
            sys.path.insert(0, str(Path(__file__).resolve().parent))
            import phase0_bench

            sys.exit(phase0_bench.main(rest))
        if i + 1 >= len(argv):
            sys.exit("--mode needs a value: report | compute")
        if argv[i + 1] != "report":
            sys.exit(f"unknown --mode value: {argv[i + 1]} (expected report | compute)")
        argv = argv[:i] + argv[i + 2:]

    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", choices=["report", "compute"], default="report",
                    help="report = legacy wall-clock page (default); compute = Phase 0 external protocol")
    ap.add_argument("--measure", action="store_true")
    ap.add_argument("--render", action="store_true")
    ap.add_argument("--runs", type=int, default=5, help="spid3r runs (best of N)")
    # 21 is the sample count tools/phase0_bench.py uses. It is not decoration:
    # the reference side of the ratio is a difference of two independently
    # measured minima, and at best-of-5 that difference moves 20-50% between
    # sessions on this host because PHP's fixed cost (~26 ms) is larger than
    # half these workloads. Best-of-21 pulls the large rows to within ~10%.
    ap.add_argument("--other-runs", type=int, default=21, help="php/python runs (best of N)")
    args = ap.parse_args(argv)

    do_measure = args.measure or not args.render
    do_render = args.render or not args.measure

    if do_measure:
        print("measuring (spid3r vm, spid3r tree, php, python)...")
        data = measure(runs_spid3r=args.runs, runs_other=args.other_runs)
        OUT_JSON.write_text(json.dumps(data, indent=2), encoding="utf-8")
        print(f"wrote {OUT_JSON.relative_to(ROOT)}")
    else:
        data = json.loads(OUT_JSON.read_text(encoding="utf-8"))

    if do_measure or not do_render:
        print_ratios(data)

    if do_render:
        from bench_report_render import render  # local module, same directory

        html = render(data, BENCH_NAMES, BENCH_DESC, ITERS, FLOORS)
        OUT_HTML.write_text(html, encoding="utf-8")
        print(f"wrote {OUT_HTML.relative_to(ROOT)} ({len(html):,} bytes)")


if __name__ == "__main__":
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    main()
