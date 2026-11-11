#!/usr/bin/env python3
"""Phase 0 external compute harness for the Spid3r performance plan.

    python tools/bench_report.py --mode compute --measure
    python tools/phase0_bench.py --measure
    python tools/phase0_bench.py --verify        # read-only, no writes
    python tools/phase0_bench.py --self-test     # sensitivity self-test only

OPTION A (security sign-off 2026-11-11): harness-side external MONOTONIC
timing with time.perf_counter_ns() around a fresh subprocess. NO language
change, NO new builtin, NO stdlib surface, NO sandbox capability decision,
NO CLI flag, NO Cargo / wasm / docs churn.

Why external timing: the .matrix sources self-time with now_ms()
(src/sched/mod.rs:1311-1326 = start.elapsed().as_millis() as i64, a wasm32
AtomicI64 call-counter fallback) and print TIME_US in 1000-microsecond steps.
That legacy field is recorded verbatim but is NEVER used for any delta; its
MAD is quantization-limited and is deliberately NOT used as the dispersion
metric. All dispersion / agreement / regression statistics use the EXTERNAL
wall_ms and the derived compute_ms series.

    compute_ms = wall_ms - startup_baseline_ms

startup_baseline_ms = median of calibration samples INTERLEAVED with the
workload runs, using the SAME binary, engine and argument shape. The
calibration program (role: calibration) does no work and prints nothing; the
systematic residual (it does not parse/compile the real workload) is disclosed
as startup_share / low_confidence and is never "corrected" by scaling the
workloads. The harness is NATIVE ONLY: on wasm32 now_ms() is a call counter
and TIME_US carries no elapsed-time information at all.

Checksum-first: every WORKLOAD run must exit 0, print exactly one pinned
RESULT line and exactly one TIME_US line, or the whole run is rejected. The
sole exception is the calibration program, excluded by an EXPLICIT LITERAL
allowlist - there is NO general "skip a missing RESULT line" branch.

Stdlib only. Fixed argv lists, no shell, no interpolation, no network.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
try:
    import phase0_manifest  # noqa: E402
    import phase0_fixtures  # noqa: E402
    from phase0_workload import busy_spin_ns  # noqa: E402
except ImportError as exc:  # fail CLOSED with a clean diagnostic, never a traceback
    sys.exit(f"PHASE0: FAIL-CLOSED - missing helper module: {exc}")

ROOT = Path(__file__).resolve().parent.parent
COMPARE = ROOT / "benchmarks" / "compare"
RESULTS = ROOT / "benchmarks" / "results.phase0.json"
CI_RESULTS = ROOT / "benchmarks" / "results.phase0.ci-advisory.json"

PINS = {
    "prime": "RESULT 5133 121013308",
    "json": "RESULT 249868800 9163",
    "map": "RESULT 44432313 100000",
}
RESULT_RE = re.compile(r"^RESULT [0-9]+ [0-9]+$")
TIME_RE = re.compile(r"^TIME_US [0-9]+$")

# EXPLICIT LITERAL ALLOWLIST. There is intentionally no general "skip a
# missing RESULT line" branch: only this one file (and its Phase 3 compiled
# form, built from the same source), with this one role.
CALIBRATION_ALLOWLIST = {
    "phase0_calibration.matrix": "calibration",
    "phase0_calibration.exe": "calibration",
}

MAD_GATE = 0.05
STARTUP_MAD_GATE_MS = 2.0
PASS_BAND = 0.03
INVALID_RERUN_BAND = 0.05
REGRESSION_FACTOR = 1.05


class GateError(Exception):
    """Checksum-first rejection: the whole run must be discarded."""


def utcnow() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def sha256_file(p: Path) -> str:
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def git_sha() -> str:
    try:
        p = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT,
                           capture_output=True, text=True, timeout=15)
        return p.stdout.strip() if p.returncode == 0 else "unknown"
    except (OSError, subprocess.TimeoutExpired):
        return "unknown"


def stats(values: list[float]) -> dict:
    """min / median / p95 (nearest-rank) / MAD / stdev / MAD-over-median."""
    xs = sorted(float(v) for v in values)
    n = len(xs)
    if n == 0:
        return {"n": 0}
    med = statistics.median(xs)
    mad = statistics.median([abs(x - med) for x in xs])
    p95_idx = max(0, min(n - 1, math.ceil(0.95 * n) - 1))
    return {
        "n": n,
        "min": xs[0],
        "max": xs[-1],
        "median": med,
        "p95": xs[p95_idx],
        "p95_method": "nearest-rank ceil(0.95*n)",
        "mad": mad,
        "stdev": statistics.pstdev(xs),
        "stdev_method": "population (statistics.pstdev)",
        "mad_over_median": (mad / med) if med > 0 else float("inf"),
    }


def checksum_gate(stdout: str, rc: int, expected: str) -> dict:
    """Checksum-first: reject ANY mismatch before a timing value is used."""
    if rc != 0:
        raise GateError(f"non-zero exit {rc}")
    lines = [ln.strip() for ln in stdout.splitlines() if ln.strip()]
    results = [ln for ln in lines if RESULT_RE.match(ln)]
    times = [ln for ln in lines if TIME_RE.match(ln)]
    if len(results) != 1:
        raise GateError(f"expected exactly one RESULT line, found {len(results)}")
    if results[0] != expected:
        raise GateError(f"RESULT {results[0]!r} != pinned {expected!r}")
    if len(times) != 1:
        raise GateError(f"expected exactly one TIME_US line, found {len(times)}")
    return {"result": results[0], "time_us": int(times[0].split()[1])}


def run_timed(cmd: list, expected: str | None, timeout: float,
              role: str = "workload", inject_ns: int = 0) -> dict:
    """Fresh subprocess, external monotonic wall clock, checksum-first gate.

    inject_ns is a harness-side busy spin INSIDE the timed region. It is used
    ONLY by the sensitivity self-test to synthesize a ~5% delta; baseline
    measurements always pass 0.
    """
    if role == "calibration":
        name = Path(str(cmd[-1])).name
        if CALIBRATION_ALLOWLIST.get(name) != "calibration":
            raise GateError(f"calibration role not allowlisted: {name}")
    elif role != "workload":
        raise GateError(f"unknown role {role!r}")

    t0 = time.perf_counter_ns()
    if inject_ns:
        busy_spin_ns(inject_ns)
    proc = subprocess.run(
        [str(c) for c in cmd], cwd=ROOT, capture_output=True,
        text=True, encoding="utf-8", errors="replace", timeout=timeout,
    )
    wall_ms = (time.perf_counter_ns() - t0) / 1e6
    out = {
        "wall_ms": wall_ms, "rc": proc.returncode, "stdout": proc.stdout,
        "stderr": proc.stderr, "role": role, "inject_ns": inject_ns,
    }
    if role == "workload":
        if expected is None:
            raise GateError("workload run requires a pinned RESULT expectation")
        out.update(checksum_gate(proc.stdout, proc.returncode, expected))
    return out


def find_binary(explicit=None) -> Path:
    if explicit:
        p = Path(explicit)
        if not p.is_file():
            raise RuntimeError(f"binary not found: {p}")
        return p
    for cfg in ("release", "debug"):
        for name in ("spid3r.exe", "spid3r"):
            p = ROOT / "target" / cfg / name
            if p.is_file():
                return p
    raise RuntimeError("no spid3r binary found; build one or pass --bin")


def pin_core(core: int | None) -> dict:
    """Best-effort process affinity pinning, fully disclosed."""
    info = {
        "pinned": False, "requested_core": core, "method": None,
        "cpuset": None, "host_isolation": "shared / unpinned host",
        "disclosure": "No CPU pinning applied. Absolute milliseconds are host-specific; "
                      "only ratios and deltas are portable.",
    }
    if core is None:
        return info
    try:
        if hasattr(os, "sched_setaffinity"):
            os.sched_setaffinity(0, {core})
            info.update(pinned=True, method="os.sched_setaffinity",
                        cpuset=sorted(os.sched_getaffinity(0)))
        elif platform.system() == "Windows":
            import ctypes
            k = ctypes.windll.kernel32
            # Declare signatures explicitly. Without argtypes/restype the HANDLE
            # pseudo-value (INVALID_HANDLE_VALUE as an unsigned pointer) is
            # silently truncated to c_int and SetProcessAffinityMask fails.
            k.GetCurrentProcess.restype = ctypes.c_void_p
            k.SetProcessAffinityMask.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
            k.SetProcessAffinityMask.restype = ctypes.c_int
            handle = k.GetCurrentProcess()
            if k.SetProcessAffinityMask(handle, 1 << core):
                info.update(pinned=True, method="SetProcessAffinityMask",
                            cpuset=[core])
        if info["pinned"]:
            info["host_isolation"] = "best-effort process-affinity pin on a shared host"
            info["disclosure"] = (
                "Process affinity pinned (children inherit), but the HOST is still shared: "
                "other processes may run on the same physical core. CPU pinning plus "
                "idle-baseline subtraction, not isolation, is what these numbers have."
            )
        else:
            info["disclosure"] = "CPU pinning was requested but could not be applied."
    except (OSError, AttributeError, ValueError) as e:
        info["disclosure"] = f"CPU pinning failed: {e}"
    return info


def idle_baseline(samples: int = 21, target_ms: float = 2.0) -> dict:
    """Host-noise proxy: busy-spin overshoot over many short intervals."""
    vals = []
    for _ in range(samples):
        t0 = time.perf_counter_ns()
        target = t0 + int(target_ms * 1e6)
        x = 0
        while time.perf_counter_ns() < target:
            x = (x * 6364136223846793005 + 1442695040888963407) & ((1 << 64) - 1)
        vals.append((time.perf_counter_ns() - t0) / 1e6)
    s = stats(vals)
    s["method"] = "busy-spin overshoot proxy (NOT a calibrated idle-baseline subtraction)"
    return s


def toolchain_info(binp: Path) -> dict:
    def capture(argv: list[str]) -> str | None:
        try:
            p = subprocess.run(argv, cwd=ROOT, capture_output=True, text=True,
                               encoding="utf-8", errors="replace", timeout=30)
            return (p.stdout or p.stderr).strip().splitlines()[0] if (p.stdout or p.stderr).strip() else None
        except (OSError, subprocess.TimeoutExpired):
            return None

    info = {
        "spid3r": capture([str(binp), "--version"]),
        "rustc": capture(["rustc", "--version"]),
        "python": platform.python_version(),
        "php": capture(["php", "-r", "echo PHP_VERSION;"]),
        "php_opcache_enable_cli": capture(["php", "-r", "echo ini_get('opcache.enable_cli');"]),
        "php_opcache_jit": capture(["php", "-r", "echo ini_get('opcache.jit');"]),
        "node": capture(["node", "--version"]),
    }
    return info


def host_info() -> dict:
    return {
        "os": platform.platform(),
        "system": platform.system(),
        "machine": platform.machine(),
        "cpu": cpu_name(),
        "cores": os.cpu_count(),
        "python": platform.python_version(),
    }


def cpu_name() -> str:
    if platform.system() == "Windows":
        try:
            out = subprocess.run(
                ["powershell", "-NoProfile", "-Command", "(Get-CimInstance Win32_Processor).Name"],
                capture_output=True, text=True, timeout=30,
            ).stdout.strip()
            if out:
                return out.splitlines()[0]
        except (OSError, subprocess.TimeoutExpired):
            pass
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    except OSError:
        pass
    return platform.processor() or "unknown CPU"


def measure_proc_startup(binp: Path, repeats: int, timeout: float) -> dict:
    """`--version` process startup. Explicitly NOT a workload; no checksum gate."""
    vals = []
    for _ in range(repeats):
        t0 = time.perf_counter_ns()
        subprocess.run([str(binp), "--version"], cwd=ROOT, capture_output=True,
                       text=True, timeout=timeout)
        vals.append((time.perf_counter_ns() - t0) / 1e6)
    return stats(vals)


class Session:
    """One measurement session with a shared, interleaved calibration series."""

    def __init__(self, binp: Path, timeout: float):
        self.binp = binp
        self.timeout = timeout
        self.tmp_obj = tempfile.TemporaryDirectory(prefix="spid3r_phase0_")
        self.tmp = Path(self.tmp_obj.name)
        committed = (COMPARE / "phase0_calibration.matrix").read_text(encoding="utf-8")
        # The calibration program is COPIED OUTSIDE the repo before execution.
        self.cal_path = self.tmp / "phase0_calibration.matrix"
        self.cal_path.write_text(committed, encoding="utf-8")
        self.cal_cmd = [binp, "run", "--engine", "vm", str(self.cal_path)]
        self.calibration: list[float] = []
        # Phase 3: the native variant runs a COMPILED calibration program (a
        # native exe of the same zero-work calibration source), so the startup
        # baseline matches the measured binary shape. Built lazily, once.
        self._native_cal: list[str] | None = None

    def native_cal_cmd(self) -> list[str]:
        if self._native_cal is None:
            exe = self.tmp / "phase0_calibration.exe"
            b = subprocess.run(
                [str(self.binp), "build", str(self.cal_path), "-o", str(exe),
                 "--release"],
                cwd=ROOT, capture_output=True, text=True, timeout=600,
            )
            if b.returncode != 0:
                raise GateError(
                    f"native calibration build failed: {b.stderr.strip()[:200]}"
                )
            self._native_cal = [str(exe)]
        return self._native_cal

    def calibrate_once(self, engine: str = "vm") -> float:
        if engine == "native-exe":
            cmd = self.native_cal_cmd()
        else:
            cmd = [self.binp, "run", "--engine", engine, str(self.cal_path)]
        r = run_timed(cmd, None, self.timeout, role="calibration")
        self.calibration.append(r["wall_ms"])
        return r["wall_ms"]

    @property
    def startup_baseline_ms(self) -> float:
        return statistics.median(self.calibration) if self.calibration else 0.0

    @property
    def startup_mad_ms(self) -> float:
        return stats(self.calibration)["mad"] if self.calibration else 0.0

    def close(self):
        self.tmp_obj.cleanup()


def measure_series(session: Session, cmd: list, expected: str, warmup: int,
                   repeats: int, label: str, inject_ns: int = 0,
                   engine: str = "vm") -> dict:
    """ONE measure()/stats() code path, shared by baselines AND selftest arms.

    Each measured iteration is INTERLEAVED with one calibration sample, so
    startup drift during the run is captured rather than assumed.
    """
    for _ in range(warmup):
        session.calibrate_once(engine)
        run_timed(cmd, expected, session.timeout, role="workload",
                  inject_ns=inject_ns)
    walls: list[float] = []
    self_us: list[int] = []
    cal_local: list[float] = []
    for _ in range(repeats):
        cal_local.append(session.calibrate_once(engine))
        r = run_timed(cmd, expected, session.timeout, role="workload",
                      inject_ns=inject_ns)
        walls.append(r["wall_ms"])
        self_us.append(r["time_us"])
    return {
        "label": label, "wall_ms": walls, "wall": stats(walls),
        "self_reported_time_us": self_us, "self_reported": stats(self_us),
        "self_reported_note": "1000-us quantized legacy field; recorded verbatim, NEVER used for deltas",
        "calibration_ms": cal_local,
        "startup_baseline_ms": statistics.median(cal_local),
        "startup_mad_ms": stats(cal_local)["mad"],
        "checksum_ok": True, "inject_ns": inject_ns, "expected": expected,
    }


def finalize_series(series: dict, startup_baseline_ms: float | None = None) -> dict:
    base = series["startup_baseline_ms"] if startup_baseline_ms is None else startup_baseline_ms
    compute = [w - base for w in series["wall_ms"]]
    series["compute_ms"] = compute
    series["compute"] = stats(compute)
    series["startup_baseline_ms_used"] = base
    series["startup_share"] = (base / series["wall"]["median"]) if series["wall"]["median"] else None
    series["low_confidence"] = bool(series["startup_share"] and series["startup_share"] > 0.50)
    return series


def build_commands(session: Session, binp: Path, workloads: list[str],
                   variants: list[str]) -> tuple[dict, list[str]]:
    cmds: dict[str, dict[str, list]] = {}
    notes: list[str] = []
    precompiled: dict[str, Path] = {}
    for w in workloads:
        src = COMPARE / f"{w}.matrix"
        all_v = {
            "vm": [binp, "run", "--engine", "vm", str(src)],
            "no-opt": [binp, "run", "--engine", "vm", "--no-opt", str(src)],
            "tree": [binp, "run", "--engine", "tree", str(src)],
        }
        if "exec" in variants:
            mc = session.tmp / f"{w}.matrixc"
            compiled = subprocess.run(
                [str(binp), "compile", str(src), "-o", str(mc)], cwd=ROOT,
                capture_output=True, text=True, timeout=session.timeout,
            )
            if compiled.returncode != 0:
                notes.append(f"{w}: compile for exec failed: {compiled.stderr.strip()[:160]}")
            else:
                precompiled[w] = mc
                all_v["exec"] = [binp, "exec", str(mc)]
        if "native" in variants:
            # Phase 3: the AOT variant runs a pre-built native executable. The
            # LLVM build happens here, ONCE, outside every timed region (the
            # legacy +25-33 ms runtime-build cost is excluded by construction);
            # `--release` matches the frozen external report's native numbers.
            exe = session.tmp / f"{w}_native.exe"
            built = subprocess.run(
                [str(binp), "build", str(src), "-o", str(exe), "--release"],
                cwd=ROOT, capture_output=True, text=True, timeout=600,
            )
            if built.returncode != 0:
                notes.append(f"{w}: native build failed: {built.stderr.strip()[:160]}")
            else:
                all_v["native"] = [str(exe)]
        cmds[w] = {v: all_v[v] for v in variants if v in all_v}
    return cmds, notes


def agreement_band(a: float, b: float) -> tuple[float, str]:
    rel = abs(a - b) / ((a + b) / 2) if (a + b) else 1.0
    if rel <= PASS_BAND:
        return rel, "PASS"
    if rel <= INVALID_RERUN_BAND:
        return rel, "INVALID_RERUN"
    return rel, "BLOCK"


def measure_baseline(args) -> dict:
    manifest_result = phase0_manifest.verify(verbose=False)
    if manifest_result["errors"]:
        raise GateError(f"manifest verification FAILED (fail-closed): {manifest_result['errors']}")

    binp = find_binary(args.bin)
    pin = pin_core(args.pin_core)
    session = Session(binp, args.timeout)
    try:
        workloads = [w.strip() for w in args.workloads.split(",") if w.strip()]
        variants = [v.strip() for v in args.variants.split(",") if v.strip()]
        cmds, notes = build_commands(session, binp, workloads, variants)

        # Blocking sensitivity self-test, through the SAME stats()/measure path.
        try:
            import phase0_selftest
        except ImportError as exc:
            raise GateError(f"sensitivity self-test module missing: {exc}") from exc
        selftest = phase0_selftest.run_all(binp, quiet=True)

        data: dict = {
            "schema": "spid3r.phase0/1",
            "generated_utc": utcnow(),
            "plan": {"file": "plan.html", "phase": 0, "commit_sha": git_sha()},
            "method": {
                "timing": "external time.perf_counter_ns around a fresh subprocess",
                "option": "A - harness-side external (no language change)",
                "warmup": args.warmup, "repeats": args.repeats, "passes": args.passes,
                "compute_ms": "wall_ms - startup_baseline_ms (median of interleaved calibration samples)",
                "self_reported": {"field": "TIME_US", "quantized_us": 1000,
                                  "authoritative": False,
                                  "note": "internal TIME_US MAD is quantization-limited and is NOT the dispersion metric"},
                "wasm": "NATIVE ONLY - wasm32 now_ms() is an AtomicI64 call counter; TIME_US carries no elapsed-time information",
                "mad_gate": MAD_GATE, "startup_mad_gate_ms": STARTUP_MAD_GATE_MS,
                "agreement_bands": {"pass": PASS_BAND, "invalid_rerun": INVALID_RERUN_BAND,
                                    "block": INVALID_RERUN_BAND},
                "regression_gate": f">{REGRESSION_FACTOR}x previous median AND >3x pooled MAD on 2 consecutive runs",
            },
            "host": host_info(),
            "toolchain": toolchain_info(binp),
            "fairness": {
                "cpu_pinning": pin,
                "idle_baseline_ms": idle_baseline(),
                "warmup": args.warmup,
                "wsl_docker_stopped": "not verified - disclosed",
                "disclosure": "Shared unpinned Windows desktop unless pinning is reported applied. "
                              "Use ratios and deltas, not absolute ms.",
            },
            "source_sha256": {e["path"]: e["sha256"] for e in manifest_result["manifest"]["sources"]},
            "manifest": [{"path": e["path"], "sha256": e["sha256"], "role": e["role"]}
                         for e in manifest_result["manifest"]["sources"]],
            "fixture_sets": manifest_result["manifest"]["fixture_sets"],
            "checksum_semantics": manifest_result["manifest"]["checksum_semantics"],
            "now_ms_impl": manifest_result["manifest"]["now_ms_impl"],
            "checksum_first": manifest_result["manifest"]["checksum_first"],
            "variants": manifest_result["manifest"]["variants"],
            "asserted_result_pins": manifest_result["manifest"]["asserted_result_pins"],
            "deviations": manifest_result["manifest"]["deviations"],
            "ci": manifest_result["manifest"]["ci"],
            "startup_proc_ms": measure_proc_startup(binp, args.repeats, args.timeout),
            "selftest": selftest,
            "workloads": {},
            "notes": notes,
            "limitations": [
                "Shared, unpinned host: absolute milliseconds are host-specific.",
                "Calibration is a zero-work program; the residual parse/compile bias is disclosed via startup_share, never corrected by scaling.",
                "native --release IS re-measured since Phase 3 (LLVM is available "
                "locally and the AOT lowering was repaired); the frozen external "
                "report numbers are kept only as historical context.",
                "Python json reference is ABSENT; geomeans vs Python cover prime and map only.",
                "External report's compute_ms came from 1-ms-quantized self-timing with 2 repeats; quoted as a finding, never a target.",
                "Docker/WSL2 1.0.0 numbers mix version and environment; never used as targets.",
                "Investigator attribution (RC3 dispatch dominates, RC4 frame allocation minor) is a written deliverable; NO hot-path edits in Phase 0.",
            ],
        }

        worst_band = "PASS"
        for w in workloads:
            data["workloads"][w] = {}
            for v, cmd in cmds[w].items():
                if v == "native":
                    # The native variant's startup baseline must match its
                    # binary shape: a compiled calibration exe, not
                    # `spid3r run`.
                    engine = "native-exe"
                else:
                    engine = "tree" if v == "tree" else "vm"
                passes = []
                for pass_no in range(1, args.passes + 1):
                    s = measure_series(session, cmd, PINS[w], args.warmup,
                                       args.repeats, f"{w}/{v}/pass{pass_no}",
                                       engine=engine)
                    finalize_series(s)
                    passes.append(s)
                rel, band = agreement_band(passes[0]["compute"]["median"],
                                           passes[-1]["compute"]["median"]) if len(passes) > 1 else (0.0, "SINGLE_PASS")
                mad_ok = all(p["compute"]["mad_over_median"] <= MAD_GATE for p in passes)
                startup_ok = all(p["startup_mad_ms"] <= STARTUP_MAD_GATE_MS for p in passes)
                if band == "BLOCK" or not mad_ok or not startup_ok:
                    worst_band = "BLOCK"
                elif band == "INVALID_RERUN" and worst_band == "PASS":
                    worst_band = "INVALID_RERUN"
                data["workloads"][w][v] = {
                    "passes": passes,
                    "median_agreement_pct": round(rel * 100, 4),
                    "numeric_band": band,
                    "mad_ok": mad_ok,
                    "startup_mad_ok": startup_ok,
                    "startup_baseline_ms": round(session.startup_baseline_ms, 4),
                    "startup_share": passes[0]["startup_share"],
                    "low_confidence": passes[0]["low_confidence"],
                    "verdict": "PASS" if (band in ("PASS", "SINGLE_PASS") and mad_ok and startup_ok)
                               else ("BLOCK" if band == "BLOCK" or not startup_ok else "INVALID_RERUN"),
                }
                if not args.quiet:
                    p0 = passes[0]
                    print(f"{w:<6} {v:<7} compute median {p0['compute']['median']:8.3f} ms  "
                          f"wall {p0['wall']['median']:8.3f} ms  "
                          f"MAD/med {p0['compute']['mad_over_median']*100:5.2f}%  "
                          f"agree {rel*100:5.2f}%  {data['workloads'][w][v]['verdict']}")

        data["startup_baseline_ms"] = round(session.startup_baseline_ms, 4)
        data["startup_mad_ms"] = round(session.startup_mad_ms, 4)
        data["startup_engine_ms"] = {
            "vm": round(session.startup_baseline_ms, 4),
            "note": "calibration runs use the same binary/engine/argument shape as the measured variant",
        }

        if not pin["pinned"]:
            status, frozen = "ADVISORY_ONLY", False
            reason = "Unpinned shared host: medians are advisory and MUST NOT be frozen as a baseline."
        elif not args.dedicated_pinned_host:
            status, frozen = "ADVISORY_ONLY", False
            reason = (
                "Best-effort process-affinity pin on a SHARED host. A3/QA require a "
                "dedicated pinned host before freezing; pass --dedicated-pinned-host "
                "only when the machine is genuinely isolated. Numeric bands below are "
                "still reported, but the baseline is not frozen."
            )
        elif worst_band == "BLOCK":
            status, frozen = "BLOCK", False
            reason = "A numeric band or MAD/startup-MAD gate failed. Invalidate the measurement; do not bisect."
        elif worst_band == "INVALID_RERUN":
            status, frozen = "INVALID_RERUN", False
            reason = "Two-run agreement fell in the 3-5% band; rerun the session."
        else:
            status, frozen = "PASS", True
            reason = "All numeric gates passed on a pinned host."
        data["verdict"] = {"status": status, "frozen": frozen,
                           "reason": reason, "numeric_band": worst_band,
                           "selftest_pass": selftest.get("pass", False)}
        if not selftest.get("pass", False):
            data["verdict"]["status"] = "BLOCK"
            data["verdict"]["frozen"] = False
            data["verdict"]["reason"] = "Sensitivity self-test FAILED; measurement is not usable."
        return data
    finally:
        session.close()


def write_artifacts(data: dict, args) -> list[Path]:
    out = Path(args.out) if args.out else (CI_RESULTS if args.advisory else RESULTS)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    written = [out]
    if not args.advisory:
        sha = data["plan"]["commit_sha"]
        baseline = {
            "schema": "spid3r.phase0.baseline/1",
            "generated_utc": data["generated_utc"],
            "commit_sha": sha,
            "frozen": data["verdict"]["frozen"],
            "status": data["verdict"]["status"],
            "reason": data["verdict"]["reason"],
            "rebaseline_rule": "Re-baseline only within +/-5% with matching checksums; never delete a floor.",
            "host": data["host"], "toolchain": data["toolchain"],
            "fairness": data["fairness"],
            "checksums": {w: PINS[w] for w in PINS},
            "source_sha256": data["source_sha256"],
            "startup_baseline_ms": data["startup_baseline_ms"],
            "startup_mad_ms": data["startup_mad_ms"],
            "medians": {
                w: {v: {"compute_ms": d["passes"][0]["compute"]["median"],
                        "wall_ms": d["passes"][0]["wall"]["median"],
                        "mad": d["passes"][0]["compute"]["mad"],
                        "p95": d["passes"][0]["compute"]["p95"],
                        "min": d["passes"][0]["compute"]["min"],
                        "stdev": d["passes"][0]["compute"]["stdev"],
                        "verdict": d["verdict"]}
                    for v, d in variants.items()}
                for w, variants in data["workloads"].items()
            },
        }
        bp = ROOT / "benchmarks" / f"baseline-{sha}.json"
        bp.write_text(json.dumps(baseline, indent=2) + "\n", encoding="utf-8")
        written.append(bp)
    return written


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--measure", action="store_true", help="run the full measurement (default)")
    ap.add_argument("--verify", action="store_true", help="read-only fixture verification")
    ap.add_argument("--self-test", action="store_true", help="sensitivity self-test only")
    ap.add_argument("--advisory", action="store_true",
                    help="write results.phase0.ci-advisory.json, never overwrite the frozen snapshot")
    ap.add_argument("--quick", action="store_true", help="warmup=2 repeats=5 passes=1 (CI advisory)")
    ap.add_argument("--bin", help="path to the spid3r binary")
    ap.add_argument("--pin-core", type=int, help="best-effort CPU affinity pin")
    ap.add_argument("--dedicated-pinned-host", action="store_true",
                    help="operator acknowledgement that the host is genuinely isolated/pinned; "
                         "required (with --pin-core) before a baseline may be frozen")
    ap.add_argument("--warmup", type=int, default=9)
    ap.add_argument("--repeats", type=int, default=21)
    ap.add_argument("--passes", type=int, default=2)
    ap.add_argument("--timeout", type=float, default=120.0)
    ap.add_argument("--variants", default="vm,no-opt,tree,exec")
    ap.add_argument("--workloads", default="prime,json,map")
    ap.add_argument("--out", help="override the result JSON path")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args(argv)

    if args.verify:
        try:
            binp = find_binary(args.bin)
        except RuntimeError as e:
            print(f"PHASE0 VERIFY: FAIL ({e})")
            return 1
        result = phase0_fixtures.verify(binp, verbose=not args.quiet)
        for e in result["errors"]:
            print(f"FAIL {e}")
        print("PHASE0 VERIFY: PASS" if not result["errors"] else "PHASE0 VERIFY: FAIL")
        return 0 if not result["errors"] else 1

    if args.self_test:
        try:
            import phase0_selftest
        except ImportError as exc:
            print(f"PHASE0 SELFTEST: FAIL (missing module: {exc})")
            return 1
        try:
            binp = find_binary(args.bin)
            result = phase0_selftest.run_all(binp, quiet=args.quiet)
        except (GateError, RuntimeError, subprocess.TimeoutExpired, OSError, ImportError) as exc:
            print(f"PHASE0 SELFTEST: FAIL ({exc})")
            return 1
        print("PHASE0 SELFTEST: " + ("PASS" if result["pass"] else "FAIL"))
        return 0 if result["pass"] else 1

    if args.quick:
        args.warmup, args.repeats, args.passes = 2, 5, 1

    try:
        data = measure_baseline(args)
    except (GateError, RuntimeError, subprocess.TimeoutExpired, OSError, ImportError) as e:
        print(f"PHASE0 MEASURE: FAIL ({e})")
        return 1
    written = write_artifacts(data, args)
    if not args.quiet:
        print(f"verdict: {data['verdict']['status']} (frozen={data['verdict']['frozen']})")
        print(f"reason : {data['verdict']['reason']}")
    for p in written:
        print(f"wrote {p.relative_to(ROOT)}")
    print("PHASE0 MEASURE: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
