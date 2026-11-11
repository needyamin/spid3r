#!/usr/bin/env python3
"""Phase 0 measurement SENSITIVITY self-test (BLOCKING CI gate).

    python tools/phase0_selftest.py
    python tools/phase0_selftest.py --json

Exit criterion from plan.html section 8: "A synthetic 5% delta on a 30 ms
workload is detectable across 5 repeats; two independent runs agree within
+/-3%".

Two layers, BOTH routed through phase0_bench's SAME stats() / run_timed()
primitives (the same code path the real baselines use):

  (a) LOGIC: deterministic, seeded mock-clock series at 30.0 ms vs 31.5 ms
      (+/-1% jitter, n=21). Asserts exact median / p95 / MAD behaviour and
      that a 5% delta is detected while a no-delta control is NOT.
  (b) PIPELINE: the REAL spid3r release binary runs the SAME verbatim HLC
      source (map.matrix, pinned RESULT 44432313 100000) in BOTH arms. Arm P
      adds a perf_counter_ns-bounded busy spin of ~5% of the measured COMPUTE
      median INSIDE the externally timed region. Arms B (baseline), P
      (injected) and C (no-delta control) are interleaved in ONE session with
      ONE shared calibration series: 9 warmup + 5 measured iterations.
      Detection uses PAIRED per-iteration differences and the arm ORDER IS
      ROTATED every iteration. Both are required on a shared host: independent
      5-sample medians leave ~1 ms of drift on a ~1.7 ms injection, and a fixed
      B,P,C order makes the control arm systematically absorb drift. With
      pairing + rotation the achieved delta is stable at ~5% and the control
      difference is ~0.1 ms.

No time.sleep (Windows timer resolution ~15.6 ms would swallow a 1.5 ms
delta). No new .matrix fixture, no scaled constants, no duplicate workload.
The injected delay is recorded as injected_ns and the ACTUALLY achieved delta
as achieved_delta_pct; a 3.5-6.5% band is required, so a silently ineffective
injection fails the test.

Verdict is BLOCKING: run_all()["pass"] must be true or the Phase 0 measurement
artifact is marked BLOCK. Two independent runs must agree.
"""

import argparse
import json
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import phase0_bench as h  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
COMPARE = ROOT / "benchmarks" / "compare"

WARMUP = 9
MEASURED = 5
LOGIC_N = 21
DELTA_BAND = (3.5, 6.5)


def detect(a: dict, b: dict) -> tuple[bool, float, float, float]:
    """|delta|/mean > 3% AND |delta| > 3 x (MAD_a + MAD_b)/2."""
    d = b["median"] - a["median"]
    mean = (a["median"] + b["median"]) / 2
    noise = 3.0 * (a["mad"] + b["mad"]) / 2.0
    rel = abs(d) / mean if mean else 1.0
    return bool(rel > 0.03 and abs(d) > noise), d, rel, noise


def logic_selftest() -> dict:
    rng = random.Random(0)
    base = [30.0 * (1.0 + rng.uniform(-0.01, 0.01)) for _ in range(LOGIC_N)]
    plus = [31.5 * (1.0 + rng.uniform(-0.01, 0.01)) for _ in range(LOGIC_N)]
    b, p = h.stats(base), h.stats(plus)
    detected, d, rel, noise = detect(b, p)
    control, cd, crel, cnoise = detect(b, h.stats(list(base)))
    ok = bool(
        abs(b["median"] - 30.0) < 0.5
        and abs(p["median"] - 31.5) < 0.5
        and p["p95"] >= p["median"] >= p["min"]
        and b["mad"] >= 0.0
        and b["mad_over_median"] <= h.MAD_GATE
        and p["mad_over_median"] <= h.MAD_GATE
        and detected and not control
    )
    return {
        "pass": ok,
        "method": "seeded deterministic mock-clock series through phase0_bench.stats()",
        "baseline_median_ms": round(b["median"], 4),
        "plus5_median_ms": round(p["median"], 4),
        "baseline": {k: b[k] for k in ("n", "min", "median", "p95", "mad", "mad_over_median")},
        "plus5": {k: p[k] for k in ("n", "min", "median", "p95", "mad", "mad_over_median")},
        "delta_detected": detected, "delta_ms": round(d, 4),
        "delta_rel": round(rel, 6), "noise_floor_ms": round(noise, 4),
        "control_detected": control, "control_delta_ms": round(cd, 4),
    }


def pipeline_once(binp: Path) -> dict:
    session = h.Session(binp, 120.0)
    cmd = [binp, "run", "--engine", "vm", str(COMPARE / "map.matrix")]
    expected = h.PINS["map"]
    try:
        # Pilot: measure the REAL workload, then size the injection at 5% of
        # COMPUTE (wall minus startup), not 5% of wall.
        pilot: list[float] = []
        for _ in range(3):
            session.calibrate_once()
            pilot.append(h.run_timed(cmd, expected, session.timeout,
                                     role="workload")["wall_ms"])
        pilot_startup = session.startup_baseline_ms
        compute_median = h.stats([w - pilot_startup for w in pilot])["median"]
        injected_ns = int(0.05 * compute_median * 1e6)

        arms = {"B": 0, "P": injected_ns, "C": 0}
        order_names = list(arms)
        walls: dict[str, list[float]] = {k: [] for k in arms}
        # ROTATE the arm order every iteration. With a fixed B,P,C order the
        # control arm C always runs last and systematically absorbs CPU drift,
        # which produced false `control_detected` on a loaded shared host.
        # Rotation cancels position effects while keeping the arms paired by
        # iteration index (each arm still runs exactly once per iteration).
        for it in range(WARMUP + MEASURED):
            session.calibrate_once()
            rot = it % len(order_names)
            order = order_names[rot:] + order_names[:rot]
            measured = it >= WARMUP
            for name in order:
                r = h.run_timed(cmd, expected, session.timeout,
                                role="workload", inject_ns=arms[name])
                if measured:
                    walls[name].append(r["wall_ms"])

        startup = session.startup_baseline_ms
        startup_mad = session.startup_mad_ms
        compute = {k: [w - startup for w in v] for k, v in walls.items()}
        st = {k: h.stats(v) for k, v in compute.items()}

        # PAIRED differences: B_i, P_i and C_i were collected in the SAME
        # iteration of ONE interleaved session, so subtracting per iteration
        # cancels the shared host drift. Comparing two independent 5-sample
        # medians instead leaves ~1 ms of scheduling noise on top of a ~1.7 ms
        # injected delta, which made the 5% detection marginal and flaky.
        diff_bp = [p - b for b, p in zip(compute["B"], compute["P"])]
        diff_bc = [c - b for b, c in zip(compute["B"], compute["C"])]
        dp = h.stats(diff_bp)
        dc = h.stats(diff_bc)
        base_median = st["B"]["median"]
        d = dp["median"]
        rel = abs(d) / base_median if base_median else 1.0
        noise = 3.0 * dp["mad"] / 2.0
        detected = bool(rel > 0.03 and d > noise)
        cd = dc["median"]
        crel = abs(cd) / base_median if base_median else 1.0
        cnoise = 3.0 * dc["mad"] / 2.0
        control = bool(crel > 0.03 and cd > cnoise)
        achieved = d / base_median * 100.0

        mad_ok = all(v["mad_over_median"] <= h.MAD_GATE for v in st.values())
        startup_ok = startup_mad <= h.STARTUP_MAD_GATE_MS
        in_band = DELTA_BAND[0] <= achieved <= DELTA_BAND[1]
        ok = bool(detected and not control and mad_ok and startup_ok and in_band)
        return {
            "pass": ok,
            "arm_binary": str(binp),
            "arm_command": [str(x) for x in cmd],
            "arm_workload": "benchmarks/compare/map.matrix (verbatim, pinned RESULT 44432313 100000)",
            "warmup": WARMUP, "measured": MEASURED,
            "injected_ns": injected_ns,
            "injection_basis": "5% of pilot COMPUTE median (wall - startup)",
            "pilot_compute_median_ms": round(compute_median, 4),
            "startup_baseline_ms": round(startup, 4),
            "startup_mad_ms": round(startup_mad, 4),
            "startup_mad_ok": startup_ok,
            "medians_compute_ms": {k: round(v["median"], 4) for k, v in st.items()},
            "mad_ms": {k: round(v["mad"], 4) for k, v in st.items()},
            "mad_over_median": {k: round(v["mad_over_median"], 6) for k, v in st.items()},
            "mad_ok": mad_ok,
            "detection_method": "paired per-iteration differences (P_i - B_i); "
                                "rel > 3% AND median(diff) > 3*MAD(diff)/2",
            "paired": {
                "bp_diff_median_ms": round(d, 4),
                "bp_diff_mad_ms": round(dp["mad"], 4),
                "bc_diff_median_ms": round(cd, 4),
                "bc_diff_mad_ms": round(dc["mad"], 4),
                "bp_diffs_ms": [round(x, 4) for x in diff_bp],
            },
            "achieved_delta_pct": round(achieved, 4),
            "delta_ms": round(d, 4), "delta_rel": round(rel, 6),
            "noise_floor_ms": round(noise, 4),
            "delta_detected": detected,
            "control_detected": control,
            "control_delta_ms": round(cd, 4),
            "achieved_in_band": in_band,
            "band_pct": list(DELTA_BAND),
        }
    finally:
        session.close()


MAX_ATTEMPTS = 3


def run_all(binp: Path, quiet: bool = True) -> dict:
    """Two independent runs per attempt; noisy attempts are INVALIDATED and RERUN.

    plan.html section 10: "MAD/median > 5% invalidates the run - rerun, do not
    bisect" and section 11: "Two runs disagreeing > 5% - invalidate the
    measurement, do not publish a verdict". This implements that rule for the
    self-test without loosening ANY threshold: an attempt is accepted only when
    both of its independent runs pass all criteria. After MAX_ATTEMPTS noisy
    attempts the verdict is FAIL (blocking), never a silent downgrade.
    """
    logic = logic_selftest()
    attempts: list[dict] = []
    accepted: list[dict] | None = None
    for attempt in range(1, MAX_ATTEMPTS + 1):
        runs = [pipeline_once(binp) for _ in range(2)]
        consistent = (
            all(r["pass"] for r in runs)
            and runs[0]["delta_detected"] == runs[1]["delta_detected"]
            and not any(r["control_detected"] for r in runs)
        )
        attempts.append({
            "attempt": attempt,
            "consistent": consistent,
            "achieved_delta_pct": [r["achieved_delta_pct"] for r in runs],
            "delta_detected": [r["delta_detected"] for r in runs],
            "control_detected": [r["control_detected"] for r in runs],
            "runs": runs,
        })
        if consistent:
            accepted = runs
            break
    result = {
        "schema": "spid3r.phase0.selftest/1",
        "blocking": True,
        "runs": 2,
        "max_attempts": MAX_ATTEMPTS,
        "attempts_used": len(attempts),
        "invalidate_rerun_policy": (
            "An attempt invalidated by host noise is discarded and rerun "
            "(plan.html sections 10-11). No threshold is loosened; after "
            "max_attempts the verdict is FAIL (blocking)."
        ),
        "logic": logic,
        "pipeline": {
            "runs": accepted if accepted is not None else attempts[-1]["runs"],
            "attempts": attempts,
            "consistent": accepted is not None,
        },
        "pass": bool(logic["pass"] and accepted is not None),
    }
    if not quiet:
        print(json.dumps(result, indent=2))
    return result


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--bin", help="path to the spid3r binary")
    ap.add_argument("--json", action="store_true", help="print the full result JSON")
    args = ap.parse_args(argv)
    try:
        binp = h.find_binary(args.bin)
    except RuntimeError as e:
        print(f"PHASE0 SELFTEST: FAIL ({e})")
        return 1
    result = run_all(binp, quiet=False)
    print("PHASE0 SELFTEST: " + ("PASS" if result["pass"] else "FAIL"))
    return 0 if result["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
