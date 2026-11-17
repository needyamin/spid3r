#!/usr/bin/env python3
"""Phase R4.2 rss_over_time 2-run baseline runner (VM & AOT native)."""

import argparse
import os
import subprocess
import sys
import tempfile
from pathlib import Path


def parse_kv(stdout: str) -> dict[str, int]:
    out: dict[str, int] = {}
    for line in stdout.splitlines():
        parts = line.strip().split()
        if len(parts) == 2 and parts[1].lstrip("-").isdigit():
            out[parts[0]] = int(parts[1])
    return out


def pct_diff(a: int, b: int) -> float:
    mid = (abs(a) + abs(b)) / 2.0
    return 0.0 if mid == 0 else abs(a - b) / mid * 100.0


def run_cmd(cmd: list[str]) -> dict[str, int]:
    res = subprocess.run(cmd, capture_output=True, text=True, check=True)
    return parse_kv(res.stdout)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/spid3r.exe")
    args = ap.parse_args()

    spid3r = str(Path(args.bin))
    workload = "benchmarks/compare/rss_over_time.matrix"

    with tempfile.TemporaryDirectory() as td:
        exe = os.path.join(td, "rss_over_time.exe" if os.name == "nt" else "rss_over_time")
        subprocess.run([spid3r, "build", "--release", workload, "-o", exe], check=True)

        aot1 = run_cmd([exe])
        aot2 = run_cmd([exe])
        vm1 = run_cmd([spid3r, "run", "--engine", "vm", workload])
        vm2 = run_cmd([spid3r, "run", "--engine", "vm", workload])

    aot_agree = pct_diff(aot1["FINAL_RSS"], aot2["FINAL_RSS"])
    vm_agree = pct_diff(vm1["FINAL_RSS"], vm2["FINAL_RSS"])
    print(f"AOT run1: {aot1}")
    print(f"AOT run2: {aot2}")
    print(f"AOT FINAL_RSS agreement: {aot_agree:.2f}%")
    print(f"VM  run1: {vm1}")
    print(f"VM  run2: {vm2}")
    print(f"VM  FINAL_RSS agreement: {vm_agree:.2f}%")

    if aot1["CHECKSUM"] != vm1["CHECKSUM"] or aot1["SAMPLES"] != 1000:
        print("FAIL: checksum or sample count mismatch", file=sys.stderr)
        return 1
    if aot_agree > 3.0 or vm_agree > 3.0:
        print("FAIL: two-run FINAL_RSS agreement > 3.0%", file=sys.stderr)
        return 1
    print("PASS: two-run agreement <= 3.0% on both AOT and VM")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
