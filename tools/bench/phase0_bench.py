#!/usr/bin/env python3
"""Phase 0 external compute harness for the Spid3r performance plan.

    python tools/bench/bench_report.py --mode compute --measure
    python tools/bench/phase0_bench.py --measure
    python tools/bench/phase0_bench.py --verify        # read-only, no writes
    python tools/bench/phase0_bench.py --self-test     # sensitivity self-test only

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

ROOT = Path(__file__).resolve().parents[2]
BENCH = ROOT / "benchmarks"
JSON_DIR = BENCH / "json"
COMPARE = BENCH / "spid3r" / "compute"
NODE_COMPARE = BENCH / "nodejs" / "compute"
RUST_COMPARE = BENCH / "rust" / "compute"
RESULTS = JSON_DIR / "compute-latest.json"
CI_RESULTS = JSON_DIR / "compute.ci-advisory.json"
SELFHOST_DIR = BENCH / "selfhost"
SELFHOST_MANIFEST = SELFHOST_DIR / "MANIFEST.sha256"
SELFHOST_BASELINE = JSON_DIR / "selfhost-baseline.json"
SELFHOST_CI_RESULTS = JSON_DIR / "selfhost.ci-advisory.json"

PINS = {
    "prime": "RESULT 5133 121013308",
    "json": "RESULT 249868800 9163",
    "map": "RESULT 44432313 100000",
    "set": "RESULT 10000 20000",
    "equality": "RESULT 4124 200000",
}
RESULT_RE = re.compile(r"^RESULT [0-9]+ [0-9]+$")
TIME_RE = re.compile(r"^TIME_US [0-9]+$")

# EXPLICIT LITERAL ALLOWLIST. There is intentionally no general "skip a
# missing RESULT line" branch: only this one file (and its Phase 3 compiled
# form, built from the same source), with this one role.
CALIBRATION_ALLOWLIST = {
    "phase0_calibration.matrix": "calibration",
    "phase0_calibration.exe": "calibration",
    "phase0_calibration.js": "calibration",
    "phase0_calibration_rs.exe": "calibration",
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
    def capture(argv: list[str], first_line: bool = True) -> str | None:
        try:
            p = subprocess.run(argv, cwd=ROOT, capture_output=True, text=True,
                               encoding="utf-8", errors="replace", timeout=30)
            text = (p.stdout or p.stderr).strip()
            if not text:
                return None
            return text.splitlines()[0] if first_line else text
        except (OSError, subprocess.TimeoutExpired):
            return None

    def spid3r_version_str() -> str | None:
        raw = capture([str(binp), "--version"], first_line=False)
        if not raw:
            return None
        ver = re.search(r"^\s*Version\s+(\S+)", raw, re.MULTILINE)
        lang = re.search(r"^\s*Language\s+(\S+)", raw, re.MULTILINE)
        if ver:
            return (
                f"spid3r {ver.group(1)} (language {lang.group(1)})"
                if lang
                else f"spid3r {ver.group(1)}"
            )
        return raw.splitlines()[0]

    info = {
        "spid3r": spid3r_version_str(),
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
        self.calibration_by_engine: dict[str, list[float]] = {}
        # Phase 3: the native variant runs a COMPILED calibration program (a
        # native exe of the same zero-work calibration source), so the startup
        # baseline matches the measured binary shape. Built lazily, once.
        self._native_cal: list[str] | None = None
        self._js_cal: list[str] | None = None
        self._rust_cal: list[str] | None = None

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

    def js_cal_cmd(self) -> list[str]:
        if self._js_cal is None:
            js_cal = self.tmp / "phase0_calibration.js"
            js_cal.write_text("", encoding="utf-8")
            node = shutil.which("node")
            if not node:
                raise GateError("node executable not found on PATH")
            self._js_cal = [node, str(js_cal)]
        return self._js_cal

    def rust_cal_cmd(self) -> list[str]:
        if self._rust_cal is None:
            rs_src = self.tmp / "phase0_calibration_rs.rs"
            rs_exe = self.tmp / "phase0_calibration_rs.exe"
            rs_src.write_text("fn main() {}\n", encoding="utf-8")
            rustc = shutil.which("rustc")
            if not rustc:
                raise GateError("rustc executable not found on PATH")
            b = subprocess.run(
                [rustc, "-O", str(rs_src), "-o", str(rs_exe)],
                cwd=ROOT, capture_output=True, text=True, timeout=600,
            )
            if b.returncode != 0:
                raise GateError(
                    f"rust calibration build failed: {b.stderr.strip()[:200]}"
                )
            self._rust_cal = [str(rs_exe)]
        return self._rust_cal

    def calibrate_once(self, engine: str = "vm") -> float:
        if engine == "native-exe":
            cmd = self.native_cal_cmd()
        elif engine == "node":
            cmd = self.js_cal_cmd()
        elif engine == "rust-exe":
            cmd = self.rust_cal_cmd()
        else:
            cmd = [self.binp, "run", "--engine", engine, str(self.cal_path)]
        r = run_timed(cmd, None, self.timeout, role="calibration")
        self.calibration.append(r["wall_ms"])
        self.calibration_by_engine.setdefault(engine, []).append(r["wall_ms"])
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
        if "node" in variants:
            js_src = NODE_COMPARE / f"{w}.js"
            node_exe = shutil.which("node")
            if not js_src.is_file():
                notes.append(f"{w}: JS reference not found ({js_src.name})")
            elif not node_exe:
                notes.append(f"{w}: node not found on PATH")
            else:
                all_v["node"] = [node_exe, str(js_src)]
        if "rust" in variants:
            rs_src = RUST_COMPARE / f"{w}.rs"
            rustc_exe = shutil.which("rustc")
            if not rs_src.is_file():
                notes.append(f"{w}: Rust reference absent ({rs_src.name})")
            elif not rustc_exe:
                notes.append(f"{w}: rustc not found on PATH")
            else:
                rs_exe = session.tmp / f"{w}_rs.exe"
                built = subprocess.run(
                    [rustc_exe, "-O", str(rs_src), "-o", str(rs_exe)],
                    cwd=ROOT, capture_output=True, text=True, timeout=600,
                )
                if built.returncode != 0:
                    notes.append(f"{w}: rust build failed: {built.stderr.strip()[:160]}")
                else:
                    all_v["rust"] = [str(rs_exe)]
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

    # Blocking sensitivity self-test before heavy compiler spawns (builds of
    # native binaries trigger AV scanning and thermal ramp on Windows).
    try:
        import phase0_selftest
    except ImportError as exc:
        raise GateError(f"sensitivity self-test module missing: {exc}") from exc
    selftest = phase0_selftest.run_all(binp, quiet=True)

    session = Session(binp, args.timeout)
    try:
        workloads = [w.strip() for w in args.workloads.split(",") if w.strip()]
        variants = [v.strip() for v in args.variants.split(",") if v.strip()]
        cmds, notes = build_commands(session, binp, workloads, variants)

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
                elif v == "node":
                    engine = "node"
                elif v == "rust":
                    engine = "rust-exe"
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

                extra_fields = {}
                eff_rel, eff_band, eff_mad_ok = rel, band, mad_ok
                if v in ("node", "rust"):
                    int_passes = [stats([u / 1000.0 for u in p["self_reported_time_us"]]) for p in passes]
                    int_rel, int_band = agreement_band(
                        int_passes[0]["median"], int_passes[-1]["median"]
                    ) if len(int_passes) > 1 else (0.0, "SINGLE_PASS")
                    int_mad_ok = all(ip["mad_over_median"] <= MAD_GATE for ip in int_passes)
                    for idx, p in enumerate(passes):
                        p["internal_compute_ms"] = int_passes[idx]
                    extra_fields["internal_compute_median_ms"] = round(int_passes[0]["median"], 4)
                    extra_fields["internal_mad_over_median_pct"] = round(int_passes[0]["mad_over_median"] * 100, 4)
                    extra_fields["internal_agreement_pct"] = round(int_rel * 100, 4)
                    extra_fields["internal_numeric_band"] = int_band
                    extra_fields["internal_mad_ok"] = int_mad_ok
                    if passes[0]["low_confidence"]:
                        extra_fields["metric_basis"] = "internal_hr_time_us (external startup_share > 50%)"
                        eff_rel, eff_band, eff_mad_ok = int_rel, int_band, int_mad_ok
                    else:
                        extra_fields["metric_basis"] = "external_compute_ms"

                # When external startup_share > 50% on node/rust, internal
                # hrtime/Instant metrics do not subtract external startup_ms.
                eff_startup_ok = True if (v in ("node", "rust") and passes[0]["low_confidence"]) else startup_ok
                if eff_band == "BLOCK" or not eff_mad_ok or not eff_startup_ok:
                    worst_band = "BLOCK"
                elif eff_band == "INVALID_RERUN" and worst_band == "PASS":
                    worst_band = "INVALID_RERUN"
                entry = {
                    "passes": passes,
                    "median_agreement_pct": round(eff_rel * 100, 4),
                    "external_median_agreement_pct": round(rel * 100, 4),
                    "numeric_band": eff_band,
                    "external_numeric_band": band,
                    "mad_ok": eff_mad_ok,
                    "external_mad_ok": mad_ok,
                    "startup_mad_ok": startup_ok,
                    "startup_baseline_ms": round(passes[0]["startup_baseline_ms"], 4),
                    "startup_share": passes[0]["startup_share"],
                    "low_confidence": passes[0]["low_confidence"],
                    "verdict": "PASS" if (eff_band in ("PASS", "SINGLE_PASS") and eff_mad_ok and eff_startup_ok)
                               else ("BLOCK" if eff_band == "BLOCK" or not eff_startup_ok else "INVALID_RERUN"),
                }
                entry.update(extra_fields)
                data["workloads"][w][v] = entry
                if not args.quiet:
                    p0 = passes[0]
                    int_note = (
                        f"  int {entry['internal_compute_median_ms']:7.3f} ms"
                        if v in ("node", "rust") else ""
                    )
                    shown_mad = (
                        entry["internal_mad_over_median_pct"]
                        if (v in ("node", "rust") and p0["low_confidence"])
                        else p0["compute"]["mad_over_median"] * 100
                    )
                    print(f"{w:<6} {v:<7} compute median {p0['compute']['median']:8.3f} ms  "
                          f"wall {p0['wall']['median']:8.3f} ms{int_note}  "
                          f"MAD/med {shown_mad:5.2f}%  "
                          f"agree {eff_rel*100:5.2f}%  {entry['verdict']}")

        data["startup_baseline_ms"] = round(session.startup_baseline_ms, 4)
        data["startup_mad_ms"] = round(session.startup_mad_ms, 4)
        startup_engines = {
            eng: round(statistics.median(vals), 4)
            for eng, vals in session.calibration_by_engine.items()
            if vals
        }
        startup_engines.setdefault("vm", round(session.startup_baseline_ms, 4))
        startup_engines["note"] = "calibration runs use the same binary/engine/argument shape as the measured variant"
        data["startup_engine_ms"] = startup_engines

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
    out = (ROOT / args.out if not Path(args.out).is_absolute() else Path(args.out)) if args.out else (CI_RESULTS if args.advisory else RESULTS)
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
        bp = JSON_DIR / f"baseline-{sha}.json"
        bp.write_text(json.dumps(baseline, indent=2) + "\n", encoding="utf-8")
        written.append(bp)
    return written


def verify_selfhost_manifest(verbose: bool = False) -> dict:
    """Fail-closed SHA-256 verification of benchmarks/selfhost/MANIFEST.sha256."""
    errors: list[str] = []
    if not SELFHOST_MANIFEST.is_file():
        return {"errors": [f"missing manifest: {SELFHOST_MANIFEST}"], "files": []}
    checked: list[dict] = []
    for raw in SELFHOST_MANIFEST.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(None, 1)
        if len(parts) != 2:
            errors.append(f"malformed manifest line: {raw!r}")
            continue
        expected_sha, rel = parts[0], parts[1].strip()
        p = ROOT / rel
        if not p.is_file():
            errors.append(f"missing pinned workload: {rel}")
            continue
        actual_sha = sha256_file(p)
        if actual_sha != expected_sha:
            errors.append(f"sha256 mismatch for {rel}: expected {expected_sha}, got {actual_sha}")
            continue
        checked.append({"path": rel, "sha256": actual_sha, "bytes": p.stat().st_size})
        if verbose:
            print(f"OK   {rel} ({p.stat().st_size} B) {actual_sha[:12]}...")
    for list_name in ("parse_check_list.txt", "corpus_build_list.txt", "selfcompile_modules.txt"):
        lp = SELFHOST_DIR / list_name
        if lp.is_file():
            for entry in lp.read_text(encoding="utf-8").splitlines():
                rel = entry.strip()
                if rel and not (ROOT / rel).is_file():
                    errors.append(f"{list_name} references missing file: {rel}")
    return {"errors": errors, "files": checked}


def _run_sh_timed(cmd: list[str], cwd: Path, timeout: float) -> tuple[float, int, str]:
    t0 = time.perf_counter_ns()
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True,
                       encoding="utf-8", errors="replace", stdin=subprocess.DEVNULL, timeout=timeout)
    ms = (time.perf_counter_ns() - t0) / 1e6
    return ms, p.returncode, p.stdout


def _sh_two_pass(label: str, warmup: int, repeats: int, passes: int,
                 step_fn, cal_fn=None, quiet: bool = False, max_attempts: int = 3) -> dict:
    """Run one 2-pass measurement, discarding and retrying up to max_attempts on INVALID_RERUN."""
    metric_key = "compute" if cal_fn is not None else "wall"
    best: dict | None = None
    for attempt in range(1, max_attempts + 1):
        for _ in range(warmup):
            step_fn()
        pass_entries: list[dict] = []
        for pno in range(1, passes + 1):
            walls: list[float] = []
            cals: list[float] = []
            for _ in range(repeats):
                if cal_fn is not None:
                    cals.append(cal_fn())
                walls.append(step_fn())
            w_st = stats(walls)
            c_base = statistics.median(cals) if cals else 0.0
            c_mad = stats(cals)["mad"] if cals else 0.0
            comp = [max(w - c_base, 1e-6) for w in walls] if cals else list(walls)
            c_st = stats(comp)
            share = (c_base / w_st["median"]) if (cals and w_st["median"] > 0) else 0.0
            pass_entries.append({
                "pass": pno,
                "wall_ms": [round(x, 4) for x in walls],
                "wall": w_st,
                "compute": c_st,
                "startup_baseline_ms": round(c_base, 4),
                "startup_mad_ms": round(c_mad, 4),
                "startup_share": share,
            })
        p0, p1 = pass_entries[0], pass_entries[-1]
        rel, band = agreement_band(p0[metric_key]["median"], p1[metric_key]["median"]) if passes > 1 else (0.0, "SINGLE_PASS")
        mad_ok = all(pe[metric_key]["mad_over_median"] <= MAD_GATE for pe in pass_entries)
        startup_ok = all(pe["startup_mad_ms"] <= STARTUP_MAD_GATE_MS for pe in pass_entries) if cal_fn is not None else True
        verdict = "PASS" if (band in ("PASS", "SINGLE_PASS") and mad_ok and startup_ok) else (
            "BLOCK" if band == "BLOCK" or not startup_ok else "INVALID_RERUN"
        )
        mad_pct = max(pe[metric_key]["mad_over_median"] for pe in pass_entries) * 100.0
        best = {
            "passes": pass_entries,
            "attempts_used": attempt,
            "median_ms": round(p0[metric_key]["median"], 4),
            "wall_median_ms": round(p0["wall"]["median"], 4),
            "startup_baseline_ms": round(p0["startup_baseline_ms"], 4),
            "startup_share": round(p0["startup_share"], 4),
            "mad_over_median_pct": round(mad_pct, 4),
            "median_agreement_pct": round(rel * 100.0, 4),
            "mad_ok": mad_ok,
            "startup_mad_ok": startup_ok,
            "verdict": verdict,
        }
        if verdict != "INVALID_RERUN":
            break
        if not quiet:
            print(f"  {label:<28} attempt {attempt}/{max_attempts} INVALID_RERUN "
                  f"(MAD/med {mad_pct:.2f}%, agree {rel*100:.2f}%), discarding & retrying")
    assert best is not None
    if not quiet:
        p0_s = best["passes"][0][metric_key]["median"]
        print(f"  {label:<28} med {p0_s:8.2f} ms  MAD/med {best['mad_over_median_pct']:5.2f}%  "
              f"agree {best['median_agreement_pct']:5.2f}%  {best['verdict']}")
    return best


def measure_selfhost_suite(args) -> dict:
    """Measure Stage-0 vs selfhost toolchain throughput on checksummed benchmarks/selfhost/."""
    manifest_res = phase0_manifest.verify(verbose=False)
    if manifest_res["errors"]:
        raise GateError(f"phase0 manifest FAILED: {manifest_res['errors']}")
    sh_man = verify_selfhost_manifest(verbose=False)
    if sh_man["errors"]:
        raise GateError(f"selfhost benchmark manifest FAILED: {sh_man['errors']}")

    binp = find_binary(args.bin)
    pin = pin_core(args.pin_core)

    try:
        import phase0_selftest
    except ImportError as exc:
        raise GateError(f"sensitivity self-test module missing: {exc}") from exc
    selftest = phase0_selftest.run_all(binp, quiet=True)
    if not args.quiet:
        print(f"[selftest] {'PASS' if selftest.get('pass') else 'FAIL'}")
    if not selftest.get("pass", False) and not args.advisory:
        raise GateError("Sensitivity self-test FAILED; aborting before 25-min suite")

    td_obj = tempfile.TemporaryDirectory(prefix="spid3r_shbench_")
    td = Path(td_obj.name)
    try:
        if args.self:
            selfp = Path(args.self)
            if not selfp.is_absolute():
                selfp = ROOT / selfp
            if not selfp.is_file():
                raise GateError(f"--self binary not found: {selfp}")
            self_build_mode = "explicit --self"
        else:
            selfp = td / ("spid3r-self.exe" if platform.system() == "Windows" else "spid3r-self")
            built = subprocess.run(
                [str(binp), "build", str(ROOT / "selfhost" / "main.matrix"), "-o", str(selfp), "--release"],
                cwd=ROOT, capture_output=True, text=True, timeout=600,
            )
            if built.returncode != 0 or not selfp.is_file():
                raise GateError(f"building release spid3r-self failed: {(built.stderr or built.stdout)[:240]}")
            self_build_mode = "stage0 build --release"

        empty = td / "empty.matrix"
        empty.write_text("", encoding="utf-8")

        lex_wl = SELFHOST_DIR / "lex_workload.matrix"
        lex_bytes = lex_wl.stat().st_size
        pos_files = [ROOT / ln.strip() for ln in (SELFHOST_DIR / "parse_check_list.txt").read_text(encoding="utf-8").splitlines() if ln.strip()]
        build_files = [ROOT / ln.strip() for ln in (SELFHOST_DIR / "corpus_build_list.txt").read_text(encoding="utf-8").splitlines() if ln.strip()]
        sc_mods = [ln.strip() for ln in (SELFHOST_DIR / "selfcompile_modules.txt").read_text(encoding="utf-8").splitlines() if ln.strip()]
        sc_bytes = sum((ROOT / r).stat().st_size for r in sc_mods)
        sc_sha = {r: sha256_file(ROOT / r) for r in sc_mods}

        requested = (
            ["lex", "parse", "check", "self_compile", "corpus_build"]
            if args.workloads == "prime,json,map,set,equality"
            else [w.strip() for w in args.workloads.split(",") if w.strip()]
        )
        timeout = max(float(args.timeout), 300.0)

        def cal_file(prefix: list[str]) -> float:
            ms, rc, _ = _run_sh_timed(prefix + [str(empty)], ROOT, timeout)
            if rc != 0:
                raise GateError(f"calibration exited {rc}: {prefix}")
            return ms

        def cal_ver(exe: Path) -> float:
            ms, rc, _ = _run_sh_timed([str(exe), "--version"], ROOT, timeout)
            if rc != 0:
                raise GateError(f"--version calibration exited {rc}: {exe}")
            return ms

        results: dict[str, dict] = {}
        worst_verdict = "PASS"

        def track_verdict(entry: dict):
            nonlocal worst_verdict
            v = entry["verdict"]
            if v == "BLOCK":
                worst_verdict = "BLOCK"
            elif v == "INVALID_RERUN" and worst_verdict == "PASS":
                worst_verdict = "INVALID_RERUN"

        if "lex" in requested:
            if not args.quiet:
                print(f"[lex] {lex_wl.relative_to(ROOT)} ({lex_bytes:,} B)")
            _, rc0, out0 = _run_sh_timed([str(binp), "tokens", "--json", str(lex_wl)], ROOT, timeout)
            _, rc1, out1 = _run_sh_timed([str(selfp), "tokens", str(lex_wl)], ROOT, timeout)
            if rc0 != 0 or rc1 != 0 or out0 != out1:
                raise GateError(f"lex parity/exit failed before measurement (rc0={rc0}, rc1={rc1}, match={out0 == out1})")

            def lex_step(cmd):
                ms, rc, _ = _run_sh_timed(cmd, ROOT, timeout)
                if rc != 0:
                    raise GateError(f"lex cmd exited {rc}")
                return ms

            s0_e = _sh_two_pass("lex / stage0", args.warmup, args.repeats, args.passes,
                                lambda: lex_step([str(binp), "tokens", "--json", str(lex_wl)]),
                                lambda: cal_file([str(binp), "tokens", "--json"]), quiet=args.quiet)
            s1_e = _sh_two_pass("lex / selfhost", args.warmup, args.repeats, args.passes,
                                lambda: lex_step([str(selfp), "tokens", str(lex_wl)]),
                                lambda: cal_file([str(selfp), "tokens"]), quiet=args.quiet)
            for e in (s0_e, s1_e):
                e["compute_mb_s"] = round(lex_bytes / (e["median_ms"] / 1000.0) / 1e6, 4)
                e["raw_mb_s"] = round(lex_bytes / (e["wall_median_ms"] / 1000.0) / 1e6, 4)
                track_verdict(e)
            results["lex"] = {
                "workload": str(lex_wl.relative_to(ROOT)).replace("\\", "/"),
                "bytes": lex_bytes, "metric": "compute_mb_s",
                "parity_verified": True, "stage0": s0_e, "selfhost": s1_e,
                "selfhost_vs_stage0": round(s1_e["compute_mb_s"] / s0_e["compute_mb_s"], 4),
            }

        if "parse" in requested:
            if not args.quiet:
                print(f"[parse] {len(pos_files)} positive corpus programs")
            for f in pos_files[:10]:
                _, rc0, out0 = _run_sh_timed([str(binp), "parse", "--dump-ast-json", str(f)], ROOT, timeout)
                _, rc1, out1 = _run_sh_timed([str(selfp), "parse", str(f)], ROOT, timeout)
                if rc0 != 0 or rc1 != 0 or out0 != out1:
                    raise GateError(f"parse precheck mismatch on {f.name}")

            def parse_batch(prefix):
                t0 = time.perf_counter_ns()
                for f in pos_files:
                    p = subprocess.run(prefix + [str(f)], cwd=ROOT, capture_output=True,
                                       stdin=subprocess.DEVNULL, timeout=timeout)
                    if p.returncode != 0:
                        raise GateError(f"parse failed rc={p.returncode} on {f.name}")
                return (time.perf_counter_ns() - t0) / 1e6

            s0_e = _sh_two_pass("parse / stage0", args.warmup, args.repeats, args.passes,
                                lambda: parse_batch([str(binp), "parse", "--dump-ast-json"]), quiet=args.quiet)
            s1_e = _sh_two_pass("parse / selfhost", args.warmup, args.repeats, args.passes,
                                lambda: parse_batch([str(selfp), "parse"]), quiet=args.quiet)
            for e in (s0_e, s1_e):
                e["programs_per_s"] = round(len(pos_files) / (e["median_ms"] / 1000.0), 3)
                track_verdict(e)
            results["parse"] = {
                "workload": "benchmarks/selfhost/parse_check_list.txt",
                "programs": len(pos_files), "metric": "programs_per_s",
                "stage0": s0_e, "selfhost": s1_e,
                "selfhost_vs_stage0": round(s1_e["programs_per_s"] / s0_e["programs_per_s"], 4),
            }

        if "check" in requested:
            if not args.quiet:
                print(f"[check] {len(pos_files)} corpus programs")

            def check_batch(prefix):
                t0 = time.perf_counter_ns()
                for f in pos_files:
                    subprocess.run(prefix + [str(f)], cwd=ROOT, capture_output=True,
                                   stdin=subprocess.DEVNULL, timeout=timeout)
                return (time.perf_counter_ns() - t0) / 1e6

            s0_e = _sh_two_pass("check / stage0", args.warmup, args.repeats, args.passes,
                                lambda: check_batch([str(binp), "check", "--dump-registry-json"]), quiet=args.quiet)
            s1_e = _sh_two_pass("check / selfhost", args.warmup, args.repeats, args.passes,
                                lambda: check_batch([str(selfp), "registry"]), quiet=args.quiet)
            for e in (s0_e, s1_e):
                e["programs_per_s"] = round(len(pos_files) / (e["median_ms"] / 1000.0), 3)
                track_verdict(e)
            results["check"] = {
                "workload": "benchmarks/selfhost/parse_check_list.txt",
                "programs": len(pos_files), "metric": "programs_per_s",
                "stage0": s0_e, "selfhost": s1_e,
                "selfhost_vs_stage0": round(s1_e["programs_per_s"] / s0_e["programs_per_s"], 4),
            }

        if "self_compile" in requested:
            if not args.quiet:
                print(f"[self_compile] selfhost/main.matrix ({len(sc_mods)} modules, {sc_bytes:,} B)")
            sc_main = str(ROOT / "selfhost" / "main.matrix")
            out_s0 = td / ("sc_s0.exe" if platform.system() == "Windows" else "sc_s0")
            out_s1 = td / ("sc_s1.exe" if platform.system() == "Windows" else "sc_s1")

            def sc_step(compiler: Path, out_exe: Path) -> float:
                ms, rc, _ = _run_sh_timed([str(compiler), "build", sc_main, "-o", str(out_exe)], ROOT, timeout)
                if rc != 0 or not out_exe.is_file() or out_exe.stat().st_size < 100_000:
                    raise GateError(f"self_compile failed rc={rc} for {compiler}")
                return ms

            s0_e = _sh_two_pass("self_compile / stage0", args.warmup, args.repeats, args.passes,
                                lambda: sc_step(binp, out_s0), lambda: cal_ver(binp), quiet=args.quiet)
            s1_e = _sh_two_pass("self_compile / selfhost", args.warmup, args.repeats, args.passes,
                                lambda: sc_step(selfp, out_s1), lambda: cal_ver(selfp), quiet=args.quiet)
            _, rc_chk0, v0 = _run_sh_timed([str(out_s0), "--version"], ROOT, timeout)
            _, rc_chk1, v1 = _run_sh_timed([str(out_s1), "--version"], ROOT, timeout)
            if rc_chk0 != 0 or rc_chk1 != 0 or v0.strip() != v1.strip():
                raise GateError(f"built selfhost --version check failed: rc0={rc_chk0} rc1={rc_chk1}")
            for e in (s0_e, s1_e):
                e["compute_mb_s"] = round(sc_bytes / (e["median_ms"] / 1000.0) / 1e6, 5)
                e["raw_mb_s"] = round(sc_bytes / (e["wall_median_ms"] / 1000.0) / 1e6, 5)
                track_verdict(e)
            results["self_compile"] = {
                "workload": "selfhost/main.matrix", "modules": len(sc_mods), "bytes": sc_bytes,
                "metric": "compute_mb_s", "stage0": s0_e, "selfhost": s1_e,
                "selfhost_vs_stage0": round(s1_e["compute_mb_s"] / s0_e["compute_mb_s"], 4),
            }

        if "corpus_build" in requested:
            if not args.quiet:
                print(f"[corpus_build] {len(build_files)} standalone examples")

            def build_batch(compiler: Path, tag: str, verify_exec: bool = False) -> dict | float:
                outs = {}
                exe_ext = ".exe" if platform.system() == "Windows" else ""
                t0 = time.perf_counter_ns()
                for f in build_files:
                    out_p = td / f"cb_{tag}_{f.stem}{exe_ext}"
                    p = subprocess.run([str(compiler), "build", str(f), "-o", str(out_p)],
                                       cwd=ROOT, capture_output=True, stdin=subprocess.DEVNULL, timeout=timeout)
                    if p.returncode != 0 or not out_p.is_file():
                        raise GateError(f"corpus_build failed rc={p.returncode} for {compiler} on {f.name}")
                    if verify_exec:
                        rp = subprocess.run([str(out_p)], cwd=ROOT, capture_output=True, text=True,
                                            encoding="utf-8", errors="replace", stdin=subprocess.DEVNULL, timeout=timeout)
                        if rp.returncode != 0:
                            raise GateError(f"built {f.name} exited {rp.returncode}")
                        outs[f.name] = rp.stdout
                ms = (time.perf_counter_ns() - t0) / 1e6
                return outs if verify_exec else ms

            o0 = build_batch(binp, "s0", verify_exec=True)
            o1 = build_batch(selfp, "s1", verify_exec=True)
            if o0 != o1:
                raise GateError("corpus_build execution output mismatch between stage0 and selfhost")

            s0_e = _sh_two_pass("corpus_build / stage0", args.warmup, args.repeats, args.passes,
                                lambda: build_batch(binp, "s0"), quiet=args.quiet)
            s1_e = _sh_two_pass("corpus_build / selfhost", args.warmup, args.repeats, args.passes,
                                lambda: build_batch(selfp, "s1"), quiet=args.quiet)
            for e in (s0_e, s1_e):
                e["programs_per_s"] = round(len(build_files) / (e["median_ms"] / 1000.0), 3)
                track_verdict(e)
            results["corpus_build"] = {
                "workload": "benchmarks/selfhost/corpus_build_list.txt",
                "programs": len(build_files), "metric": "batch_ms",
                "parity_verified": True, "stage0": s0_e, "selfhost": s1_e,
                "selfhost_vs_stage0_time": round(s1_e["median_ms"] / s0_e["median_ms"], 4),
            }

        targets = {}
        if "lex" in results:
            r = results["lex"]
            targets["lex"] = {
                "metric": "compute_mb_s",
                "stage0_baseline": r["stage0"]["compute_mb_s"],
                "selfhost_baseline": r["selfhost"]["compute_mb_s"],
                "selfhost_vs_stage0": r["selfhost_vs_stage0"],
                "tier_a_floor_mb_s": round(r["selfhost"]["compute_mb_s"] * 0.8, 3),
            }
        if "parse" in results:
            r = results["parse"]
            targets["parse"] = {
                "metric": "programs_per_s",
                "stage0_baseline": r["stage0"]["programs_per_s"],
                "selfhost_baseline": r["selfhost"]["programs_per_s"],
                "selfhost_vs_stage0": r["selfhost_vs_stage0"],
                "tier_a_floor_programs_s": round(r["selfhost"]["programs_per_s"] * 0.8, 1),
            }
        if "check" in results:
            r = results["check"]
            targets["check"] = {
                "metric": "programs_per_s",
                "stage0_baseline": r["stage0"]["programs_per_s"],
                "selfhost_baseline": r["selfhost"]["programs_per_s"],
                "selfhost_vs_stage0": r["selfhost_vs_stage0"],
                "tier_a_floor_programs_s": round(r["selfhost"]["programs_per_s"] * 0.8, 1),
            }
        if "self_compile" in results:
            r = results["self_compile"]
            targets["self_compile"] = {
                "metric": "compute_mb_s",
                "stage0_baseline": r["stage0"]["compute_mb_s"],
                "selfhost_baseline": r["selfhost"]["compute_mb_s"],
                "selfhost_vs_stage0": r["selfhost_vs_stage0"],
                "tier_a_floor_mb_s": round(r["selfhost"]["compute_mb_s"] * 0.8, 4),
            }
        if "corpus_build" in results:
            r = results["corpus_build"]
            targets["corpus_build"] = {
                "metric": "batch_ms",
                "stage0_baseline_ms": r["stage0"]["median_ms"],
                "selfhost_baseline_ms": r["selfhost"]["median_ms"],
                "selfhost_vs_stage0_time": r["selfhost_vs_stage0_time"],
                "tier_a_max_batch_ms": round(r["selfhost"]["median_ms"] * 1.25, 1),
            }

        status = "ADVISORY_ONLY" if not (pin["pinned"] and args.dedicated_pinned_host) else worst_verdict
        if not selftest.get("pass", False):
            status = "BLOCK"
        return {
            "schema": "spid3r.selfhost.bench/1",
            "generated_utc": utcnow(),
            "plan": {"file": "docs/selfhost/SELFHOST.md", "phase": "11.P-2", "commit_sha": git_sha()},
            "protocol": {
                "warmup": args.warmup, "repeats": args.repeats, "passes": args.passes,
                "mad_gate": MAD_GATE, "agreement_band_pass": PASS_BAND,
                "startup_mad_gate_ms": STARTUP_MAD_GATE_MS,
            },
            "host": host_info(),
            "toolchain": {
                **toolchain_info(binp),
                "selfhost_build_mode": self_build_mode,
                "selfhost_bin_sha256": sha256_file(selfp),
            },
            "fairness": {"cpu_pinning": pin},
            "manifest": sh_man["files"],
            "selfhost_source_sha256": sc_sha,
            "selftest": selftest,
            "workloads": results,
            "targets": targets,
            "verdict": {
                "status": status,
                "frozen": status == "PASS",
                "numeric_band": worst_verdict,
                "selftest_pass": selftest.get("pass", False),
            },
        }
    finally:
        td_obj.cleanup()


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--measure", action="store_true", help="run the full measurement (default)")
    ap.add_argument("--verify", action="store_true", help="read-only fixture verification")
    ap.add_argument("--verify-selfhost", action="store_true",
                    help="read-only SHA-256 verification of benchmarks/selfhost/MANIFEST.sha256")
    ap.add_argument("--selfhost-bench", action="store_true",
                    help="measure Stage-0 vs selfhost toolchain throughput on benchmarks/selfhost/")
    ap.add_argument("--self", help="path to selfhost compiler binary (defaults to building stage0 --release)")
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
    ap.add_argument("--workloads", default="prime,json,map,set,equality")
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
            pin_core(args.pin_core)
            result = phase0_selftest.run_all(binp, quiet=args.quiet)
        except (GateError, RuntimeError, subprocess.TimeoutExpired, OSError, ImportError) as exc:
            print(f"PHASE0 SELFTEST: FAIL ({exc})")
            return 1
        print("PHASE0 SELFTEST: " + ("PASS" if result["pass"] else "FAIL"))
        return 0 if result["pass"] else 1

    if args.verify_selfhost:
        res = verify_selfhost_manifest(verbose=not args.quiet)
        for e in res["errors"]:
            print(f"FAIL {e}")
        print("SELFHOST BENCH VERIFY: PASS" if not res["errors"] else "SELFHOST BENCH VERIFY: FAIL")
        return 0 if not res["errors"] else 1

    if args.quick:
        args.warmup, args.repeats, args.passes = 2, 5, 1

    if args.selfhost_bench:
        try:
            data = measure_selfhost_suite(args)
        except (GateError, RuntimeError, subprocess.TimeoutExpired, OSError, ImportError) as e:
            print(f"SELFHOST BENCH: FAIL ({e})")
            return 1
        default_out = SELFHOST_CI_RESULTS if args.advisory else SELFHOST_BASELINE
        out = (ROOT / args.out if not Path(args.out).is_absolute() else Path(args.out)) if args.out else default_out
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
        v = data["verdict"]
        if not args.quiet:
            print(f"verdict: {v['status']} (frozen={v['frozen']}, band={v['numeric_band']}, selftest={v['selftest_pass']})")
            print(f"wrote {out.relative_to(ROOT)}")
        ok = bool(v["selftest_pass"] and v["numeric_band"] == "PASS")
        print("SELFHOST BENCH: PASS" if ok else f"SELFHOST BENCH: FAIL ({v['status']} / {v['numeric_band']})")
        return 0 if (ok or args.advisory) else 1

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
