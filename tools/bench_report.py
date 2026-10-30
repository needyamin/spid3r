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


def startup_ms(cmd, runs=5):
    return best_of(cmd, runs)


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

    data["startup"]["spid3r"] = startup_ms([binp, "--version"])
    if shutil.which("php"):
        data["startup"]["php"] = startup_ms(["php", "-r", ""])
    if shutil.which("python"):
        data["startup"]["python"] = startup_ms([sys.executable, "-c", ""])
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

    return data


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
    ap = argparse.ArgumentParser()
    ap.add_argument("--measure", action="store_true")
    ap.add_argument("--render", action="store_true")
    ap.add_argument("--runs", type=int, default=5, help="spid3r runs (best of N)")
    ap.add_argument("--other-runs", type=int, default=3, help="php/python runs (best of N)")
    args = ap.parse_args()

    do_measure = args.measure or not args.render
    do_render = args.render or not args.measure

    if do_measure:
        print("measuring (spid3r vm, spid3r tree, php, python)...")
        data = measure(runs_spid3r=args.runs, runs_other=args.other_runs)
        OUT_JSON.write_text(json.dumps(data, indent=2), encoding="utf-8")
        print(f"wrote {OUT_JSON.relative_to(ROOT)}")
    else:
        data = json.loads(OUT_JSON.read_text(encoding="utf-8"))

    if do_render:
        from bench_report_render import render  # local module, same directory

        html = render(data, BENCH_NAMES, BENCH_DESC, ITERS, FLOORS)
        OUT_HTML.write_text(html, encoding="utf-8")
        print(f"wrote {OUT_HTML.relative_to(ROOT)} ({len(html):,} bytes)")


if __name__ == "__main__":
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    main()
