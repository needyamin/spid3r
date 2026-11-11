#!/usr/bin/env python3
"""Verify the Phase 0 integrity anchor FAIL-CLOSED, before any execution.

    python tools/phase0_manifest.py --verify
    python tools/phase0_manifest.py --write   # regenerate SOURCES.sha256

Every vendored file is hashed and cross-checked against BOTH manifest.json and
SOURCES.sha256. A missing file, a digest mismatch, an unmanifested ledger entry,
a bad role or a missing licence attestation is a hard failure: the measurement
harness calls verify() first and refuses to run anything if it returns errors.

Stdlib only. No third-party packages. No repo writes in --verify mode.
"""

import argparse
import hashlib
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
COMPARE = ROOT / "benchmarks" / "compare"
MANIFEST = COMPARE / "manifest.json"
LEDGER = COMPARE / "SOURCES.sha256"

EXPECTED_ROLES = {"workload", "reference", "calibration"}


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_manifest() -> dict:
    return json.loads(MANIFEST.read_text(encoding="utf-8"))


def verify(verbose: bool = True) -> dict:
    errors: list[str] = []
    m = load_manifest()
    manifest_map = {e["path"]: e["sha256"] for e in m["sources"]}
    roles = {e["path"]: e["role"] for e in m["sources"]}

    ledger_map: dict[str, str] = {}
    if not LEDGER.is_file():
        errors.append("SOURCES.sha256 is missing")
    else:
        for line in LEDGER.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            parts = line.split()
            if len(parts) != 2 or len(parts[0]) != 64:
                errors.append(f"malformed ledger line: {line!r}")
                continue
            ledger_map[parts[1]] = parts[0]

    checked = 0
    for path, expected in sorted(manifest_map.items()):
        p = COMPARE / path
        if not p.is_file():
            errors.append(f"missing file: {path}")
            continue
        got = sha256_file(p)
        if got != expected:
            errors.append(f"digest mismatch {path}: got {got}, manifest says {expected}")
        if ledger_map.get(path) != expected:
            errors.append(f"SOURCES.sha256 mismatch for {path}")
        if roles[path] not in EXPECTED_ROLES:
            errors.append(f"unknown role {roles[path]!r} for {path}")
        checked += 1
        if verbose:
            print(f"OK   {path:<28} {got[:12]}  role={roles[path]}")

    extra = sorted(set(ledger_map) - set(manifest_map))
    if extra:
        errors.append(f"ledger lists unmanifested files: {extra}")

    # Exactly three timed workloads + one calibration fixture, nothing else.
    workloads = sorted(p for p, r in roles.items() if r == "workload")
    calibrations = sorted(p for p, r in roles.items() if r == "calibration")
    if workloads != ["json.matrix", "map.matrix", "prime.matrix"]:
        errors.append(f"unexpected workload set: {workloads}")
    if calibrations != ["phase0_calibration.matrix"]:
        errors.append(f"unexpected calibration set: {calibrations}")

    # Pins must be present and absolute.
    for w, pin in {
        "prime": "RESULT 5133 121013308",
        "json": "RESULT 249868800 9163",
        "map": "RESULT 44432313 100000",
    }.items():
        if m["workloads"][w]["expected_result"] != pin:
            errors.append(f"{w}: pinned RESULT changed")

    prov = m.get("provenance", {})
    # The vendored HLC fixtures start as `OPEN` / `unverified` (owner
    # attestation only). Once the repository owner makes the GPL/provenance
    # decision, the manifest records `CONFIRMED` / `confirmed-by-owner` —
    # the attestation field is REQUIRED in both states (fail-closed: a
    # missing or empty attestation always fails verification).
    if prov.get("license_status") not in ("OPEN", "CONFIRMED") or not prov.get(
        "owner_attestation"
    ):
        errors.append("missing licence/provenance owner attestation")
    if prov.get("license") not in ("unverified", "confirmed-by-owner"):
        errors.append(
            "licence must be explicitly unverified or owner-confirmed"
        )
    if prov.get("license_status") == "CONFIRMED" and "CONFIRM" not in prov.get(
        "owner_attestation", ""
    ).upper():
        errors.append(
            "CONFIRMED licence requires the owner-confirmation record in the attestation"
        )

    if verbose:
        print(f"---- {checked} files verified, {len(errors)} error(s)")
        for e in errors:
            print(f"FAIL {e}")
    return {"checked": checked, "errors": errors, "manifest": m,
            "workloads": workloads, "calibrations": calibrations}


def write_ledger() -> int:
    m = load_manifest()
    lines = [f"{e['sha256']}  {e['path']}" for e in sorted(m["sources"], key=lambda x: x["path"])]
    LEDGER.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"wrote {LEDGER.relative_to(ROOT)} ({len(lines)} entries)")
    return 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--verify", action="store_true")
    ap.add_argument("--write", action="store_true")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args(argv)
    if args.write and not args.verify:
        return write_ledger()
    result = verify(verbose=not args.quiet)
    if result["errors"]:
        print("MANIFEST VERIFY: FAIL")
        return 1
    print("MANIFEST VERIFY: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
