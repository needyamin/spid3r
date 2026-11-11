#!/usr/bin/env python3
"""Read-only Phase 0 fixture verifier (blocking CI gate).

    python tools/phase0_fixtures.py --verify
    python tools/phase0_fixtures.py --verify --bin target/release/spid3r

Four independent assertions, all of them NEW coverage. NO existing gate is
modified and NO stdout normalization is performed inside any existing gate:

  1. FAIL-CLOSED integrity: every vendored file matches manifest.json and
     SOURCES.sha256 (delegated to phase0_manifest).
  2. MECHANICAL DERIVATIVE PROOF: each inline golden-value `Case` source in
     tests/engine_matrix.rs is byte-exactly its verbatim .matrix source with
     exactly three timing lines removed - `let t0 = now_ms();`,
     `let us = (now_ms() - t0) * 1000;`, `println("TIME_US ...")`.
     Additionally, canonical-formatting that derived source (spid3r fmt) must
     still produce the pinned RESULT, so formatting cannot silently change the
     workload. The inline string itself is intentionally NOT fmt-formatted:
     it is a Rust string literal, not a .matrix file, and `spid3r fmt --check`
     never sees it.
  3. ABSOLUTE PINS on the verbatim sources across tree / vm / exec: exit 0,
     identical first stderr line, exactly one ^TIME_US [0-9]+$ line, RESULT
     line equal to the absolute pinned string, and the remaining stdout
     byte-identical across all three engines.
  4. The inline RESULT-only sources produce exactly the pinned RESULT on tree
     and vm with empty stderr.

Stdlib only. No repo writes: all derived sources and bytecode live in a temp
directory. Fixed argv lists only; no shell, no interpolation.
"""

import argparse
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import phase0_manifest  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
COMPARE = ROOT / "benchmarks" / "compare"
ENGINE_MATRIX = ROOT / "tests" / "engine_matrix.rs"

PINS = {
    "prime": "RESULT 5133 121013308",
    "json": "RESULT 249868800 9163",
    "map": "RESULT 44432313 100000",
}
TIME_RE = re.compile(r"^TIME_US [0-9]+$")
RESULT_RE = re.compile(r"^RESULT [0-9]+ [0-9]+$")
CASE_RE = re.compile(
    r'name:\s*"phase0_hlc_(prime|json|map)"\s*,\s*src:\s*r#"(.*?)"#\s*,',
    re.S,
)
TIMING_LINES = (
    "let t0 = now_ms();",
    "let us = (now_ms() - t0) * 1000;",
)


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


def run(argv, timeout=120) -> subprocess.CompletedProcess:
    return subprocess.run(
        [str(a) for a in argv], cwd=ROOT, capture_output=True,
        text=True, encoding="utf-8", errors="replace", timeout=timeout,
    )


def strip_timing(text: str, workload: str) -> tuple[str, list[str]]:
    """Remove EXACTLY the three timing lines. Reject any other count."""
    errors: list[str] = []
    out, removed = [], []
    for line in text.splitlines(keepends=True):
        s = line.strip()
        if s in TIMING_LINES or s.startswith('println("TIME_US '):
            removed.append(s)
            continue
        out.append(line)
    expected = [
        "let t0 = now_ms();",
        "let us = (now_ms() - t0) * 1000;",
        'println("TIME_US " + string(us));',
    ]
    if removed != expected:
        errors.append(
            f"{workload}: expected exactly 3 timing lines {expected}, removed {removed}"
        )
    return "".join(out), errors


def extract_inline_cases() -> tuple[dict[str, str], list[str]]:
    text = ENGINE_MATRIX.read_text(encoding="utf-8")
    found = {m.group(1): m.group(2) for m in CASE_RE.finditer(text)}
    errors = [w for w in PINS if w not in found]
    if errors:
        errors = [f"missing inline phase0_hlc_* Case row for: {errors}"]
    return found, errors


def strict_normalize(stdout: str) -> tuple[list[str] | None, str | None]:
    """Remove EXACTLY ONE TIME_US line. Reject 0 or >1. Reject nothing else."""
    lines = stdout.splitlines()
    idx = [i for i, line in enumerate(lines) if TIME_RE.match(line)]
    if len(idx) != 1:
        return None, f"expected exactly one TIME_US line, found {len(idx)}"
    remaining = lines[: idx[0]] + lines[idx[0] + 1:]
    return remaining, None


def verify(binp: Path, verbose: bool = True) -> dict:
    errors: list[str] = []
    checks = 0

    # 1. FAIL-CLOSED integrity anchor, BEFORE any execution.
    manifest_result = phase0_manifest.verify(verbose=False)
    errors.extend(f"manifest: {e}" for e in manifest_result["errors"])
    checks += manifest_result["checked"]
    if errors:
        return {"checks": checks, "errors": errors, "binary": str(binp)}
    if verbose:
        print(f"OK   manifest: {checks} vendored files verified (fail-closed)")

    inline, e = extract_inline_cases()
    errors.extend(e)
    if errors:
        return {"checks": checks, "errors": errors, "binary": str(binp)}

    with tempfile.TemporaryDirectory(prefix="spid3r_phase0_verify_") as td:
        tmp = Path(td)
        for workload, pin in PINS.items():
            verbatim_path = COMPARE / f"{workload}.matrix"
            verbatim = verbatim_path.read_text(encoding="utf-8")

            # 2. Mechanical derivative proof (byte-exact 3-line delta).
            derived, e = strip_timing(verbatim, workload)
            errors.extend(e)
            if inline[workload] != derived:
                errors.append(
                    f"{workload}: inline Case src != verbatim minus exactly 3 timing lines"
                )
            if 'println("TIME_US ' in inline[workload]:
                errors.append(f"{workload}: inline Case src still prints TIME_US")
            # Do NOT assert that the pinned RESULT *digits* appear literally in
            # the source: they are COMPUTED values (prime 121013308, json 9163),
            # not literals. `inline == derived` above is byte-exact and already
            # guarantees constants, loop bodies and the RESULT expression.
            # Assert only that the RESULT print expression survived the delta.
            if 'println("RESULT "' not in derived:
                errors.append(f"{workload}: RESULT println expression missing from derived src")
            expected_const = str(
                manifest_result["manifest"]["workloads"][workload]["constants"].get("limit", "")
            )
            if expected_const and expected_const not in derived:
                errors.append(f"{workload}: pinned constant {expected_const} missing from derived src")
            if verbose:
                print(f"OK   {workload}: inline Case src == verbatim - 3 timing lines (byte-exact)")

            # canonical_format check: formatting the derived source must not
            # change the pinned RESULT. The inline string stays raw on purpose.
            fmt_path = tmp / f"{workload}_derived.matrix"
            fmt_path.write_text(derived, encoding="utf-8")
            fmt = run([binp, "fmt", str(fmt_path)])
            if fmt.returncode != 0:
                errors.append(f"{workload}: spid3r fmt failed on derived source")
            # The inline Rust string is intentionally NOT fmt-formatted; only
            # pin preservation under canonical formatting is asserted here.
            out = run([binp, "run", "--engine", "vm", str(fmt_path)])
            if out.returncode != 0 or out.stdout.strip() != pin:
                errors.append(
                    f"{workload}: canonical_format(derived) produced "
                    f"{out.stdout.strip()!r} (expected {pin!r}, rc={out.returncode})"
                )
            elif verbose:
                print(f"OK   {workload}: canonical_format(derived) keeps pinned RESULT")
            checks += 2

            # 3. Absolute pins on the VERBATIM sources, tree / vm / exec.
            mc = tmp / f"{workload}.matrixc"
            compiled = run([binp, "compile", str(verbatim_path), "-o", str(mc)])
            if compiled.returncode != 0:
                errors.append(f"{workload}: compile failed: {compiled.stderr.strip()[:120]}")
            commands = {
                "tree": [binp, "run", "--engine", "tree", str(verbatim_path)],
                "vm": [binp, "run", "--engine", "vm", str(verbatim_path)],
                "exec": [binp, "exec", str(mc)],
            }
            normalized: dict[str, list[str]] = {}
            stderrs: dict[str, str] = {}
            for engine, cmd in commands.items():
                p = run(cmd)
                if p.returncode != 0:
                    errors.append(f"{workload}/{engine}: exit {p.returncode}: {p.stderr.strip()[:120]}")
                    continue
                results = [ln for ln in p.stdout.splitlines() if RESULT_RE.match(ln)]
                if results != [pin]:
                    errors.append(f"{workload}/{engine}: RESULT {results!r} != {pin!r}")
                    continue
                rem, why = strict_normalize(p.stdout)
                if why:
                    errors.append(f"{workload}/{engine}: {why}")
                    continue
                normalized[engine] = rem
                stderrs[engine] = p.stderr.splitlines()[0].strip() if p.stderr.splitlines() else ""
                checks += 1
            if len(normalized) == 3:
                if not (normalized["tree"] == normalized["vm"] == normalized["exec"]):
                    errors.append(f"{workload}: normalized stdout differs across tree/vm/exec")
                if len(set(stderrs.values())) != 1:
                    errors.append(f"{workload}: first stderr line differs across engines: {stderrs}")
            if verbose:
                print(f"OK   {workload}: absolute pin + exactly-one-TIME_US + 3-engine parity")

            # 4. Inline RESULT-only sources: deterministic stdout on tree + vm.
            inline_path = tmp / f"{workload}_inline.matrix"
            inline_path.write_text(inline[workload], encoding="utf-8")
            for engine in ("tree", "vm"):
                p = run([binp, "run", "--engine", engine, str(inline_path)])
                if p.returncode != 0 or p.stdout.strip() != pin or p.stderr.strip():
                    errors.append(
                        f"{workload}/{engine}: inline source produced "
                        f"rc={p.returncode} stdout={p.stdout.strip()!r} stderr={p.stderr.strip()!r}"
                    )
                checks += 1
            if verbose:
                print(f"OK   {workload}: inline RESULT-only src deterministic on tree + vm")

    return {"checks": checks, "errors": errors, "binary": str(binp),
            "canonical_note": "canonical_format is asserted semantically (same pinned RESULT); "
                              "the inline Rust string intentionally stays byte-exact raw"}


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--verify", action="store_true")
    ap.add_argument("--bin", help="path to the spid3r binary")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args(argv)
    try:
        binp = find_binary(args.bin)
    except RuntimeError as e:
        print(f"FIXTURE VERIFY: FAIL ({e})")
        return 1
    result = verify(binp, verbose=not args.quiet)
    for e in result["errors"]:
        print(f"FAIL {e}")
    if result["errors"]:
        print(f"FIXTURE VERIFY: FAIL ({len(result['errors'])} error(s))")
        return 1
    print(f"FIXTURE VERIFY: PASS ({result['checks']} checks, binary {binp})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
