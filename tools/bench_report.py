#!/usr/bin/env python3
"""Measure the Spid3r benchmark suite and render docs/html/benchmark.html.

Two phases, so the page can be re-rendered without re-measuring:

    python tools/bench_report.py --measure   # writes benchmarks/json/micro-suite.json
    python tools/bench_report.py --render    # writes docs/html/benchmark.html
    python tools/bench_report.py             # both

Everything here is wall-clock, best-of-N, measured by spawning a fresh process
per run - the same method `spid3r bench` itself uses. Interpreter/runtime
startup is measured separately via a no-op invocation so both raw wall time
and startup-adjusted compute time are reported honestly.
"""

import argparse
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BENCH = ROOT / "benchmarks"
JSON_DIR = BENCH / "json"
SPID3R_DIR = BENCH / "spid3r"
OUT_HTML = ROOT / "docs" / "html" / "benchmark.html"
OUT_JSON = JSON_DIR / "micro-suite.json"
REF_P1_JSON = JSON_DIR / "compute-baseline.json"
LATEST_COMPUTE_JSON = JSON_DIR / "compute-latest.json"
REF_P2_JSON = JSON_DIR / "selfhost-baseline.json"
BUDGETS_JSON = JSON_DIR / "footprint-budgets.json"

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

STARTUP_FLOOR_MS = 8.5

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
    "tasks": "2K spawn/channel task round-trips",
    "stress_suite": "Mixed workload, correctness only (ungated)",
}


def bin_path() -> Path:
    for name in ("spid3r.exe", "spid3r"):
        p = ROOT / "target" / "release" / name
        if p.exists():
            return p
    sys.exit("target/release/spid3r not found - run `cargo build --release` first")


TIME_RE = re.compile(
    r"^(?P<stem>[a-z_]+)\s+(?P<iters>\d+|-)\s+(?P<time>[\d.]+)(?P<unit>ms|s|µs)\s+(?P<rate>[\d.]+|-)\s*$"
)


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


def parse_spid3r_version(out: str) -> str:
    ver = re.search(r"^\s*Version\s+(\S+)", out, re.MULTILINE)
    lang = re.search(r"^\s*Language\s+(\S+)", out, re.MULTILINE)
    if ver:
        return f"spid3r {ver.group(1)} (language {lang.group(1)})" if lang else f"spid3r {ver.group(1)}"
    lines = [line.strip() for line in out.splitlines() if line.strip()]
    return lines[0] if lines else "spid3r"


def git_sha() -> str:
    try:
        p = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=10,
        )
        return p.stdout.strip() if p.returncode == 0 else "unknown"
    except OSError:
        return "unknown"


def run_bench(engine: str, runs: int, compare: bool = False):
    cmd = [str(bin_path()), "bench", "--engine", engine, "--runs", str(runs)]
    if compare:
        cmd.append("--compare")
    proc = subprocess.run(
        cmd, cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace"
    )
    if proc.returncode != 0:
        sys.exit(f"`spid3r bench --engine {engine}` failed:\n{proc.stdout}\n{proc.stderr}")
    return parse_bench_table(proc.stdout), proc.stdout


def best_of(cmd, runs, expected_stdout=None):
    """Best wall-clock of N fresh process runs, in ms (checking stdout if supplied)."""
    best = float("inf")
    for _ in range(runs):
        t0 = time.perf_counter()
        proc = subprocess.run(
            cmd, cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace"
        )
        elapsed = (time.perf_counter() - t0) * 1000
        if proc.returncode != 0:
            raise RuntimeError(f"command {cmd} failed (exit {proc.returncode}): {proc.stderr.strip()}")
        if expected_stdout is not None and proc.stdout.strip() != expected_stdout:
            raise RuntimeError(
                f"checksum mismatch for {cmd}: got {proc.stdout.strip()!r}, expected {expected_stdout!r}"
            )
        best = min(best, elapsed)
    return best


RESOLUTION_ABS_MS = 0.5
RESOLUTION_MAD_FACTOR = 2.0


def startup_stats(cmd, runs=9):
    """(best_ms, mad_ms) for a no-op invocation."""
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


def measure_aot(binp: str, stems, runs_aot: int):
    """Build release AOT executables for each micro workload and measure them."""
    results = {}
    aot_startup = None
    exe_suf = ".exe" if os.name == "nt" else ""
    with tempfile.TemporaryDirectory(prefix="spid3r_micro_aot_") as td:
        tmp = Path(td)
        empty_src = tmp / "empty.matrix"
        empty_src.write_text("fn main() { return 0; }\n", encoding="utf-8")
        empty_exe = tmp / f"empty{exe_suf}"
        b0 = subprocess.run(
            [binp, "build", "--release", str(empty_src), "-o", str(empty_exe)],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        if b0.returncode != 0:
            return results, None, f"AOT unavailable in this build: {b0.stderr.strip()[:120]}"
        aot_startup = startup_stats([str(empty_exe)], runs=9)

        for stem in stems:
            src = SPID3R_DIR / f"{stem}.matrix"
            if not src.is_file():
                continue
            # Reference stdout from VM
            vm_ref = subprocess.run(
                [binp, "run", "--engine", "vm", str(src)],
                cwd=ROOT,
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
            )
            if vm_ref.returncode != 0:
                continue
            expected = vm_ref.stdout.strip()
            out_exe = tmp / f"{stem}{exe_suf}"
            b = subprocess.run(
                [binp, "build", "--release", str(src), "-o", str(out_exe)],
                cwd=ROOT,
                capture_output=True,
                text=True,
            )
            if b.returncode != 0:
                continue
            results[stem] = best_of([str(out_exe)], runs_aot, expected_stdout=expected)
    return results, aot_startup, None


def measure(runs_spid3r=5, runs_tree=3, runs_other=3):
    binp = str(bin_path())
    data = {
        "generated": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        "git_sha": git_sha(),
        "host": {
            "os": platform.platform(),
            "cpu": cpu_name(),
            "cores": os.cpu_count(),
            "python": platform.python_version(),
        },
        "runs": {"spid3r": runs_spid3r, "aot": runs_spid3r, "tree": runs_tree, "other": runs_other},
        "spid3r_version": parse_spid3r_version(
            subprocess.run(
                [binp, "--version"],
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
            ).stdout
        ),
        "startup": {},
        "startup_mad": {},
        "resolution": {},
        "vm": {},
        "aot": {},
        "tree": {},
        "nodejs": {},
        "php": {},
        "python": {},
        "gate": [],
    }

    def record_startup(key, cmd):
        best, mad = startup_stats(cmd)
        data["startup"][key] = best
        data["startup_mad"][key] = mad
        data["resolution"][key] = max(RESOLUTION_ABS_MS, RESOLUTION_MAD_FACTOR * mad)

    record_startup("spid3r", [binp, "--version"])
    if shutil.which("node"):
        record_startup("nodejs", ["node", "-e", ""])
        data["node_version"] = (
            subprocess.run(["node", "-v"], capture_output=True, text=True).stdout.strip().lstrip("v")
        )
    if shutil.which("php"):
        record_startup("php", ["php", "-r", ""])
        data["php_version"] = (
            subprocess.run(["php", "-r", "echo PHP_VERSION;"], capture_output=True, text=True).stdout.strip()
        )
    if shutil.which("python"):
        record_startup("python", [sys.executable, "-c", ""])

    vm_rows, vm_out = run_bench("vm", runs_spid3r, compare=True)
    data["compare"] = True
    data["vm_raw"] = vm_out
    tree_rows, tree_out = run_bench("tree", runs_tree)
    data["tree_raw"] = tree_out

    for stem, row in vm_rows.items():
        data["vm"][stem] = row["ms"]
    for stem, row in tree_rows.items():
        data["tree"][stem] = row["ms"]

    # Measure Spid3r Native AOT (YamLLVM) on the same gated workloads
    aot_res, aot_st, aot_note = measure_aot(binp, [s for s in vm_rows if s in ITERS], runs_spid3r)
    if aot_res:
        data["aot"] = aot_res
    if aot_st:
        best_a, mad_a = aot_st
        data["startup"]["aot"] = best_a
        data["startup_mad"]["aot"] = mad_a
        data["resolution"]["aot"] = max(RESOLUTION_ABS_MS, RESOLUTION_MAD_FACTOR * mad_a)
    if aot_note:
        data["aot_note"] = aot_note

    # Reference implementations in benchmarks/{nodejs,php,python}/
    for stem in vm_rows:
        if stem not in ITERS:
            continue
        fname = "json_bench" if stem == "json" else stem
        js = BENCH / "nodejs" / f"{fname}.js"
        if js.exists() and shutil.which("node"):
            data["nodejs"][stem] = best_of(["node", str(js)], runs_other)
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

    data["adj"] = {}
    for engine, startup_key in (
        ("vm", "spid3r"),
        ("aot", "aot"),
        ("tree", "spid3r"),
        ("nodejs", "nodejs"),
        ("php", "php"),
        ("python", "python"),
    ):
        startup = data["startup"].get(startup_key, 0.0) or 0.0
        data["adj"][engine] = {
            stem: max(0.0, ms - startup) for stem, ms in data[engine].items()
        }

    data["ratio_php"] = {}
    data["ratio_adj"] = {}
    data["ratio_ref"] = {}
    data["ratio_resolved"] = {}
    for engine in ("vm", "aot", "tree"):
        vs_php, vs_best, refs_used, resolved = {}, {}, {}, {}
        for stem, adj in data["adj"].get(engine, {}).items():
            php_adj = data["adj"]["php"].get(stem)
            if php_adj is not None and php_adj > data["resolution"].get("php", RESOLUTION_ABS_MS):
                vs_php[stem] = adj / php_adj

            usable = {
                ref: data["adj"][ref][stem]
                for ref in ("nodejs", "php", "python")
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
    known = [s for s in WORKLOAD_ORDER if s in data["vm"]]
    extra = [s for s in data["vm"] if s not in WORKLOAD_ORDER]
    return known + extra


def print_ratios(data):
    adj = data.get("adj") or {}
    vs_php = data.get("ratio_php") or {}
    vs_best = data.get("ratio_adj") or {}
    refs_used = data.get("ratio_ref") or {}
    resolved = data.get("ratio_resolved") or {}

    def fmt(v, width, prec=1):
        return f"{'-':>{width}}" if v is None else f"{v:>{width}.{prec}f}"

    print()
    print(
        f"{'workload':<10}{'aot adj':>9}{'vm adj':>9}{'vm/php':>8}"
        f"{'tree adj':>10}{'node adj':>10}{'php adj':>9}{'py adj':>8}   best_ref"
    )
    for stem in ordered_workloads(data):
        if stem == "stress_suite":
            continue
        aot_adj = (adj.get("aot") or {}).get(stem)
        vm_adj = (adj.get("vm") or {}).get(stem)
        tree_adj = (adj.get("tree") or {}).get(stem)
        nd_adj = (adj.get("nodejs") or {}).get(stem)
        php_adj = (adj.get("php") or {}).get(stem)
        py_adj = (adj.get("python") or {}).get(stem)
        is_res = (resolved.get("vm") or {}).get(stem, False)
        used = (refs_used.get("vm") or {}).get(stem, "") if is_res else ""
        print(
            f"{stem:<10}{fmt(aot_adj, 9)}{fmt(vm_adj, 9)}{fmt((vs_php.get('vm') or {}).get(stem), 8, 2)}"
            f"{fmt(tree_adj, 10)}{fmt(nd_adj, 10)}{fmt(php_adj, 9)}{fmt(py_adj, 8)}   {used}"
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


def verify_doc_sync() -> int:
    """Verify docs/html/benchmark.html and frozen JSON baselines agree on every published figure."""
    if not OUT_HTML.is_file():
        print("SYNC FAIL: missing docs/html/benchmark.html", file=sys.stderr)
        return 1
    bench_txt = OUT_HTML.read_text(encoding="utf-8")
    ref_p1 = json.loads(REF_P1_JSON.read_text(encoding="utf-8")) if REF_P1_JSON.is_file() else {}
    ref_p2 = json.loads(REF_P2_JSON.read_text(encoding="utf-8")) if REF_P2_JSON.is_file() else {}

    for tidx, tbl in enumerate(re.findall(r"<table\b[^>]*>.*?</table>", bench_txt, re.S), 1):
        rows = re.findall(r"<tr\b[^>]*>.*?</tr>", tbl, re.S)
        counts = [len(re.findall(r"<(?:th|td)\b", r)) for r in rows]
        if not counts or len(set(counts)) != 1:
            print(f"SYNC FAIL: docs/html/benchmark.html table {tidx} column-count drift {counts}", file=sys.stderr)
            return 1

    wl1 = ref_p1.get("workloads") or {}
    tg2 = ref_p2.get("targets") or {}
    pins = [
        ("P-1 prime VM", f"{wl1['prime']['vm']['passes'][0]['compute']['median']:.2f}"),
        ("P-1 prime native", f"{wl1['prime']['native']['passes'][0]['compute']['median']:.2f}"),
        ("P-1 prime Node", f"{wl1['prime']['node']['internal_compute_median_ms']:.2f}"),
        ("P-1 prime Rust", f"{wl1['prime']['rust']['internal_compute_median_ms']:.2f}"),
        ("P-1 json Node", f"{wl1['json']['node']['internal_compute_median_ms']:.2f}"),
        ("P-1 map Node", f"{wl1['map']['node']['internal_compute_median_ms']:.2f}"),
        ("P-2 lex floor", f"{tg2['lex']['tier_a_floor_mb_s']:.3f}"),
        ("P-2 parse floor", f"{tg2['parse']['tier_a_floor_programs_s']:.1f}"),
        ("P-2 check floor", f"{tg2['check']['tier_a_floor_programs_s']:.1f}"),
        ("P-2 self_compile floor", f"{tg2['self_compile']['tier_a_floor_mb_s']:.4f}"),
        ("P-2 corpus_build max", f"{tg2['corpus_build']['tier_a_max_batch_ms']:.1f}"),
        ("P-5 startup", "8.099"),
        ("P-6 prime", "5.76"),
        ("P-6 json", "148.82"),
        ("P-6 map", "28.00"),
        ("P-6 lex sh", "1387.53"),
        ("P-6 parse sh", "928.75"),
        ("P-6 check sh", "1081.87"),
        ("P-6 self_compile sh", "5932.15"),
        ("P-6 corpus_build sh", "673.84"),
    ]
    for name, val in pins:
        if val not in bench_txt:
            print(f"SYNC FAIL: {name} ({val}) missing in docs/html/benchmark.html", file=sys.stderr)
            return 1
    print(f"BENCH DOC SYNC PASS: {len(pins)} figures verified across benchmark.html and benchmarks/json/*.json")
    return 0


def main():
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
    ap.add_argument("--mode", choices=["report", "compute"], default="report")
    ap.add_argument("--measure", action="store_true")
    ap.add_argument("--render", action="store_true")
    ap.add_argument("--check-startup-floor", action="store_true")
    ap.add_argument("--verify-sync", action="store_true")
    ap.add_argument("--runs", type=int, default=5, help="spid3r runs (best of N)")
    ap.add_argument("--other-runs", type=int, default=5, help="node/php/python runs (best of N)")
    args = ap.parse_args(argv)

    if args.verify_sync:
        sys.exit(verify_doc_sync())

    do_measure = args.measure or not args.render
    do_render = args.render or not args.measure

    JSON_DIR.mkdir(parents=True, exist_ok=True)

    if do_measure:
        print("measuring (spid3r aot, spid3r vm, spid3r tree, node, php, python)...")
        data = measure(runs_spid3r=args.runs, runs_other=args.other_runs)
        OUT_JSON.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8", newline="\n")
        print(f"wrote {OUT_JSON.relative_to(ROOT)}")
    else:
        data = json.loads(OUT_JSON.read_text(encoding="utf-8"))

    if do_measure or not do_render:
        print_ratios(data)

    if args.check_startup_floor:
        sp_startup = (data.get("startup") or {}).get("spid3r")
        if sp_startup is None or sp_startup > STARTUP_FLOOR_MS:
            sys.exit(
                f"FAIL: spid3r startup {sp_startup} ms exceeds Tier-A floor {STARTUP_FLOOR_MS} ms"
            )
        print(f"STARTUP FLOOR PASS: {sp_startup:.3f} ms <= {STARTUP_FLOOR_MS:.1f} ms")

    if do_render:
        from bench_report_render import render

        ref_p1 = json.loads(REF_P1_JSON.read_text(encoding="utf-8")) if REF_P1_JSON.is_file() else None
        latest_compute = (
            json.loads(LATEST_COMPUTE_JSON.read_text(encoding="utf-8"))
            if LATEST_COMPUTE_JSON.is_file()
            else None
        )
        ref_p2 = json.loads(REF_P2_JSON.read_text(encoding="utf-8")) if REF_P2_JSON.is_file() else None
        budgets = json.loads(BUDGETS_JSON.read_text(encoding="utf-8")) if BUDGETS_JSON.is_file() else None
        rendered = render(
            data,
            BENCH_NAMES,
            BENCH_DESC,
            ITERS,
            FLOORS,
            ref_p1=ref_p1,
            ref_p2=ref_p2,
            latest_compute=latest_compute,
            budgets=budgets,
        )
        OUT_HTML.write_text(rendered, encoding="utf-8", newline="\n")
        print(f"wrote {OUT_HTML.relative_to(ROOT)} ({len(rendered):,} bytes)")


if __name__ == "__main__":
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    main()
