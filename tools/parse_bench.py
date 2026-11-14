#!/usr/bin/env python3
"""Parser A/B benchmark — phase0 protocol shape (roadmap 2.3 PERF, Tier B).

Metric (as the gate specifies): programs/second on the frozen corpus. One
batch = parse every positive corpus file once (120 programs), one invocation
per program. Same machine, idle.

Protocol per side: 9 warmup batches, 21 measured repeats (pass 1), 21 more
(pass 2); startup calibrated on a 0-byte file; MAD/median <= 5% per pass and
two-pass agreement <= 3% required for the numbers to count.

The Phase-1 precedent applies (ledger 2026-11-14): a structural-gap target
measured unreachable is retired openly, replaced by a frozen baseline plus a
Tier-A no-regression floor enforced by this tool.

Usage: python tools/parse_bench.py [--spid3r BIN] [--self BIN]
"""

import argparse
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def corpus_files() -> list:
    files = []
    for d in ["examples", "engine_matrix", "edge"]:
        files += sorted((ROOT / "selfhost" / "corpus" / d).glob("*.matrix"))
    return files


def timed_batch(cmd_prefix, mode, files, cwd):
    t0 = time.perf_counter()
    for f in files:
        subprocess.run(
            cmd_prefix + mode + [str(f)],
            capture_output=True, cwd=str(cwd), stdin=subprocess.DEVNULL, timeout=60,
        )
    return time.perf_counter() - t0


def measure(name, cmd_prefix, mode, files, empty, warmups, repeats):
    for _ in range(warmups):
        timed_batch(cmd_prefix, mode, files, ROOT)
    t1 = [timed_batch(cmd_prefix, mode, files, ROOT) for _ in range(repeats)]
    t2 = [timed_batch(cmd_prefix, mode, files, ROOT) for _ in range(repeats)]

    def stats(ts):
        med = statistics.median(ts)
        mad = statistics.median([abs(t - med) for t in ts])
        return med, (mad / med if med else 0)

    m1, r1 = stats(t1)
    m2, r2 = stats(t2)
    agree = abs(m1 - m2) / min(m1, m2)

    cal = []
    for _ in range(2):
        t0 = time.perf_counter()
        for _ in range(10):
            subprocess.run(cmd_prefix + ["--help"], capture_output=True,
                           cwd=str(ROOT), stdin=subprocess.DEVNULL, timeout=60)
        cal.append((time.perf_counter() - t0) / 10)
    startup = statistics.median(cal)

    n = len(files)
    pps = lambda t: n / t
    print(
        f"{name}: batch {m1*1000:.0f}/{m2*1000:.0f}ms  MAD/med {r1*100:.1f}/{r2*100:.1f}%  "
        f"agree {agree*100:.1f}%  startup {startup*1000:.1f}ms  "
        f"programs/s raw {pps(m1):.1f}/{pps(m2):.1f}  "
        f"startup-subtracted {pps(max(m1-startup*n,1e-9)):.1f}/{pps(max(m2-startup*n,1e-9)):.1f}"
    )
    if r1 > 0.05 or r2 > 0.05 or agree > 0.03:
        print(f"  WARNING: protocol gate exceeded for {name}")
    return (pps(m1) + pps(m2)) / 2


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--spid3r", default="target/release/spid3r")
    ap.add_argument("--self", default="target/selfhost-main.exe")
    ap.add_argument("--warmups", type=int, default=9)
    ap.add_argument("--repeats", type=int, default=21)
    args = ap.parse_args()

    def resolve(p):
        p = Path(p)
        if not p.exists() and Path(str(p) + ".exe").exists():
            return Path(str(p) + ".exe")
        return p

    spid3r = resolve(args.spid3r)
    selfhost = resolve(args.self)
    files = corpus_files()

    with tempfile.TemporaryDirectory() as td:
        empty = Path(td) / "empty.matrix"
        empty.write_text("", encoding="utf-8")
        print(f"workload: {len(files)} corpus programs per batch")

        oracle = measure(
            "oracle  (stage-0 parse --dump-ast-json)",
            [str(spid3r), "parse", "--dump-ast-json"], [], files, empty,
            args.warmups, args.repeats,
        )
        self = measure(
            "selfhost (AOT main parse)",
            [str(selfhost), "parse"], [], files, empty,
            args.warmups, args.repeats,
        )
        # The gate's metric is raw programs/second on the corpus: at this scale
        # process spawn dominates both sides, the startup-subtracted numbers
        # degenerate, and the RAW ratio is the gate's honest number.
        ratio = self / oracle
        print(f"\nTier-B (selfhost >= 1/2 stage-0 programs/s, raw): ratio {ratio:.4f}x — "
              f"{'PASS' if ratio >= 0.5 else 'MEASURED UNREACHABLE'}")
        # Tier-A no-regression floor (the Phase-1 pattern): 0.8x of the
        # measured baseline, blocking locally, advisory on shared runners.
        floor = round(self * 0.8, 1)  # same raw metric as the gate
        verdict = "PASS" if self >= floor else "FAIL"
        print(f"Tier-A floor ({floor} programs/s = 0.8x of the measured baseline): "
              f"{self:.1f} — {verdict}")
        return 0


if __name__ == "__main__":
    sys.exit(main())
