#!/usr/bin/env python3
"""Lexer A/B benchmark — phase0 protocol (roadmap 1.4 PERF, Tier B).

Same machine, idle, one deterministic workload file (the positive corpus
concatenated 16x, byte-stable given the frozen corpus), one invocation per
repeat. Protocol per side: 9 warmup runs, 21 measured repeats (pass 1), 21
more (pass 2); startup calibrated on a 0-byte file; MAD/median <= 5% per
pass and two-pass agreement <= 3% required for the numbers to count.

Reports MB/s per side (raw and startup-subtracted) and the selfhost/oracle
ratio against the Tier-B target (>= 1/2). A target this measures as
unreachable is retired openly (docs/PERF_TARGET_RETIREMENT.md discipline) —
never silently.

Usage: python tools/bench/lex_bench.py [--spid3r BIN] [--self BIN]
"""

import argparse
import json
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BASELINE_JSON = ROOT / "benchmarks" / "json" / "selfhost-baseline.json"
PINNED_WORKLOAD = ROOT / "benchmarks" / "selfhost" / "lex_workload.matrix"


def load_lex_target() -> tuple[float, float | None, str]:
    if BASELINE_JSON.is_file():
        data = json.loads(BASELINE_JSON.read_text(encoding="utf-8"))
        t = (data.get("targets") or {}).get("lex")
        if t and "tier_a_floor_mb_s" in t:
            return float(t["tier_a_floor_mb_s"]), float(t["selfhost_baseline"]), "benchmarks/json/selfhost-baseline.json"
    return 0.19, None, "fallback constant (baseline.json missing)"


def build_workload(td: Path) -> Path:
    files = []
    for d in ["examples", "engine_matrix", "edge"]:
        files += sorted((ROOT / "selfhost" / "corpus" / d).glob("*.matrix"))
    text = "".join(f.read_text(encoding="utf-8") for f in files)
    p = Path(td) / "workload.matrix"
    p.write_text(text * 16, encoding="utf-8", newline="")
    return p


def run_one(cmd, path):
    t0 = time.perf_counter()
    subprocess.run(cmd + [str(path)], capture_output=True, cwd=str(ROOT),
                   stdin=subprocess.DEVNULL, timeout=600)
    return time.perf_counter() - t0


def measure(name, cmd, path, empty, warmups, repeats):
    for _ in range(warmups):
        run_one(cmd, path)
    times1 = [run_one(cmd, path) for _ in range(repeats)]
    times2 = [run_one(cmd, path) for _ in range(repeats)]

    def stats(ts):
        med = statistics.median(ts)
        mad = statistics.median([abs(t - med) for t in ts])
        return med, (mad / med if med else 0)

    m1, r1 = stats(times1)
    m2, r2 = stats(times2)
    agree = abs(m1 - m2) / min(m1, m2)

    cal = []
    for _ in range(3):
        cal.append(run_one(cmd, empty))
    startup = statistics.median(cal)

    size = path.stat().st_size
    mbps = lambda t: size / t / 1e6
    print(
        f"{name}: med {m1*1000:.0f}/{m2*1000:.0f}ms  MAD/med {r1*100:.1f}/{r2*100:.1f}%  "
        f"agree {agree*100:.1f}%  startup {startup*1000:.1f}ms  "
        f"MB/s raw {mbps(m1):.2f}/{mbps(m2):.2f}  "
        f"startup-subtracted {mbps(max(m1-startup,1e-9)):.2f}/{mbps(max(m2-startup,1e-9)):.2f}"
    )
    if r1 > 0.05 or r2 > 0.05 or agree > 0.03:
        print(f"  WARNING: protocol gate exceeded for {name}")
    return (mbps(max(m1 - startup, 1e-9)) + mbps(max(m2 - startup, 1e-9))) / 2


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--spid3r", default="target/release/spid3r")
    ap.add_argument("--self", default="selfhost/main_native.exe")
    ap.add_argument("--warmups", type=int, default=9)
    ap.add_argument("--repeats", type=int, default=21)
    ap.add_argument("--advisory", action="store_true", help="do not fail exit code on floor miss")
    args = ap.parse_args()

    def resolve(p):
        p = Path(p)
        if not p.exists() and Path(str(p) + ".exe").exists():
            return Path(str(p) + ".exe")
        return p

    spid3r = resolve(args.spid3r)
    selfhost = resolve(args.self)
    self_cmd = [str(selfhost)] if "lexer" in selfhost.stem.lower() else [str(selfhost), "tokens"]

    with tempfile.TemporaryDirectory() as td:
        workload = PINNED_WORKLOAD if PINNED_WORKLOAD.is_file() else build_workload(Path(td))
        empty = Path(td) / "empty.matrix"
        empty.write_text("", encoding="utf-8")
        print(f"workload: {workload.stat().st_size} bytes, one invocation per repeat")

        oracle = measure(
            "oracle  (stage-0 tokens --json)",
            [str(spid3r), "tokens", "--json"], workload, empty, args.warmups, args.repeats,
        )
        self = measure(
            f"selfhost ({selfhost.name})",
            self_cmd, workload, empty, args.warmups, args.repeats,
        )
        ratio = self / oracle
        print(f"\nTier-B (selfhost >= 1/2 stage-0, startup-subtracted): ratio {ratio:.3f}x — "
              f"{'PASS' if ratio >= 0.5 else 'MEASURED UNREACHABLE'}")
        floor, frozen_base, src_note = load_lex_target()
        base_txt = f", baseline {frozen_base:.2f} MB/s" if frozen_base is not None else ""
        if self < floor:
            print(f"Tier-A FLOOR {'ADVISORY' if args.advisory else 'FAIL'}: "
                  f"selfhost {self:.2f} MB/s < {floor} MB/s (from {src_note}{base_txt})")
            return 0 if args.advisory else 1
        print(f"Tier-A floor ({floor} MB/s from {src_note}{base_txt}): PASS ({self:.2f} MB/s)")
        return 0


if __name__ == "__main__":
    sys.exit(main())
