#!/usr/bin/env python3
"""Busy-spin helper for the Phase 0 sensitivity self-test.

Deliberately NOT a workload, NOT a benchmark fixture and NOT a .matrix file.
The self-test injects a ~5% compute delta by calling busy_spin_ns() INSIDE the
externally timed region of one arm, while BOTH arms run the SAME real spid3r
binary on the SAME verbatim HLC source with the SAME pinned constants.

A perf_counter_ns-bounded integer spin is used instead of time.sleep(): on
Windows the default timer resolution (~15.6 ms) would make a 1.5 ms / 5% delta
on a 30 ms workload unmeasurable. No time.sleep anywhere.

    python tools/phase0_workload.py --self-check
"""

import argparse
import time

MASK = (1 << 64) - 1


def busy_spin_ns(ns: int) -> int:
    """Burn approximately `ns` nanoseconds of CPU in the calling process."""
    if ns <= 0:
        return 0
    end = time.perf_counter_ns() + ns
    x = 0
    while time.perf_counter_ns() < end:
        x = (x * 6364136223846793005 + 1442695040888963407) & MASK
    return x


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--self-check", action="store_true",
                    help="verify a 30 ms + 5% spin lands within tolerance")
    args = ap.parse_args()
    if not args.self_check:
        ap.error("nothing to do; pass --self-check")
    t0 = time.perf_counter_ns()
    busy_spin_ns(30_000_000)
    base = (time.perf_counter_ns() - t0) / 1e6
    t0 = time.perf_counter_ns()
    busy_spin_ns(31_500_000)
    plus = (time.perf_counter_ns() - t0) / 1e6
    pct = (plus - base) / base * 100.0
    ok = 28.0 <= base <= 34.0 and 2.0 <= pct <= 8.0
    print(f"base={base:.3f} ms  plus={plus:.3f} ms  delta={pct:.2f}%  {'PASS' if ok else 'FAIL'}")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
