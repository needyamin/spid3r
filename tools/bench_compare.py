#!/usr/bin/env python3
"""
tools/bench_compare.py - Multi-language benchmark runner and comparative analyzer.
Compares Spid3r (Tree-walk, Bytecode VM, and Compiled .matrixc) against Python 3 and PHP.
Generates terminal output and updates the official docs/html/benchmarks.html interactive report.
"""

import sys
import os
import re
import time
import subprocess
import json
import shutil
from pathlib import Path

import platform


def _ensure_php_on_path():
    r"""Make `php` discoverable without hardcoding one machine's layout.

    Order: an explicit `SPID3R_PHP_DIR`, then whatever is already on PATH, then
    the usual Windows install locations. The old code baked in one host's
    `C:\Program Files\PHP\v8.3`, which is a property of that machine, not of
    the project (audit A-18).
    """
    explicit = os.environ.get("SPID3R_PHP_DIR")
    if explicit and Path(explicit).is_dir():
        os.environ["Path"] = explicit + os.pathsep + os.environ.get("Path", "")
        return
    if shutil.which("php"):
        return
    if platform.system() == "Windows":
        for version in ("8.4", "8.3", "8.2", "8.1"):
            candidate = Path(r"C:\Program Files\PHP") / f"v{version}"
            if candidate.is_dir():
                os.environ["Path"] = str(candidate) + os.pathsep + os.environ.get("Path", "")
                return


_ensure_php_on_path()

BENCHMARKS = [
    {
        "id": "arith",
        "name": "Arithmetic & Loop",
        "desc": "Tight loop of 2M iterations accumulating integer additions (tests loop & ALU throughput)",
        "expected": "1999999000000",
        "spid3r": "benchmarks/arith.matrix",
        "python": "benchmarks/python/arith.py",
        "php": "benchmarks/php/arith.php",
    },
    {
        "id": "list",
        "name": "List Churn",
        "desc": "Push 100K elements to dynamic array then pop all elements summing their values",
        "expected": "4999950000",
        "spid3r": "benchmarks/list.matrix",
        "python": "benchmarks/python/list.py",
        "php": "benchmarks/php/list.php",
    },
    {
        "id": "map",
        "name": "Map / Dict Operations",
        "desc": "Insert 20K key-value pairs into a hash table and look up all keys",
        "expected": "199990000",
        "spid3r": "benchmarks/map.matrix",
        "python": "benchmarks/python/map.py",
        "php": "benchmarks/php/map.php",
    },
    {
        "id": "strings",
        "name": "String Concatenation",
        "desc": "Sequential concatenation building a 30,000-character string in a loop",
        "expected": "30000",
        "spid3r": "benchmarks/strings.matrix",
        "python": "benchmarks/python/strings.py",
        "php": "benchmarks/php/strings.php",
    },
    {
        "id": "closures",
        "name": "Closure Creation & Calls",
        "desc": "Create and invoke 20,000 heap-allocated closures capturing lexical environments",
        "expected": "399980000",
        "spid3r": "benchmarks/closures.matrix",
        "python": "benchmarks/python/closures.py",
        "php": "benchmarks/php/closures.php",
    },
    {
        "id": "methods",
        "name": "Class Method Dispatch",
        "desc": "Instantiate an object and invoke an incrementing instance method 200,000 times",
        "expected": "200000",
        "spid3r": "benchmarks/methods.matrix",
        "python": "benchmarks/python/methods.py",
        "php": "benchmarks/php/methods.php",
    },
    {
        "id": "json",
        "name": "JSON Parse & Stringify",
        "desc": "Serialize 2,000 structured nested maps/lists to JSON strings and parse them back",
        "expected": "1999000",
        "spid3r": "benchmarks/json.matrix",
        "python": "benchmarks/python/json_bench.py",
        "php": "benchmarks/php/json_bench.php",
    },
    {
        "id": "tasks",
        "name": "Concurrency / Channels",
        "desc": "Producer-consumer message passing across tasks/threads transmitting 2,000 messages",
        "expected": "3998000",
        "spid3r": "benchmarks/tasks.matrix",
        "python": "benchmarks/python/tasks.py",
        "php": "benchmarks/php/tasks.php",
    },
]

def find_binary(name):
    # Check current directory / target/release first for spid3r
    if name == "spid3r":
        if "SPID3R_BIN" in os.environ and Path(os.environ["SPID3R_BIN"]).exists():
            return str(Path(os.environ["SPID3R_BIN"]).resolve())
        rel = Path("target/release/spid3r.exe")
        if rel.exists():
            return str(rel.resolve())
        dbg = Path("target/debug/spid3r.exe")
        if dbg.exists():
            return str(dbg.resolve())
    return shutil.which(name)

def run_cmd(cmd):
    start = time.perf_counter()
    res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    dur = time.perf_counter() - start
    out = res.stdout.strip()
    return dur, out, res.returncode, res.stderr

def measure(cmd, runs=3):
    times = []
    last_out = None
    for _ in range(runs):
        dur, out, code, err = run_cmd(cmd)
        if code != 0:
            return None, f"Error (code {code}): {err.strip()}"
        times.append(dur)
        last_out = out
    # Take the best (minimum) run time to filter out operating system scheduling jitter
    return min(times), last_out

def main():
    runs = 3
    custom_bin = None
    i = 1
    while i < len(sys.argv):
        arg = sys.argv[i]
        if arg in ("--bin", "-b") and i + 1 < len(sys.argv):
            custom_bin = sys.argv[i + 1]
            i += 2
        elif arg.isdigit():
            runs = int(arg)
            i += 1
        else:
            i += 1

    spid3r_bin = custom_bin or find_binary("spid3r")
    python_bin = sys.executable or find_binary("python")
    php_bin = find_binary("php")

    print(f"=== Spid3r vs Python vs PHP Benchmark Suite (Best of {runs} runs) ===")
    print(f"Spid3r binary: {spid3r_bin}")
    print(f"Python binary: {python_bin}")
    print(f"PHP binary:    {php_bin}")
    print("-" * 75)

    if not spid3r_bin:
        print("ERROR: Spid3r binary not found. Build with `cargo build --release` first.")
        sys.exit(1)

    # Pre-compile matrix files to test bytecode exec
    compiled_files = {}
    for b in BENCHMARKS:
        src = b["spid3r"]
        stem = Path(src).stem
        out_matrixc = f"benchmarks/{stem}.matrixc"
        subprocess.run([spid3r_bin, "compile", src, "-o", out_matrixc], check=True)
        compiled_files[b["id"]] = out_matrixc

    results = []

    # Get system environment versions
    py_ver = subprocess.run([python_bin, "--version"], capture_output=True, text=True).stdout.strip()
    php_ver_raw = subprocess.run([php_bin, "-v"], capture_output=True, text=True).stdout.splitlines()[0] if php_bin else "N/A"
    spid3r_ver = subprocess.run([spid3r_bin, "--version"], capture_output=True, text=True).stdout.strip()

    for b in BENCHMARKS:
        bid = b["id"]
        bname = b["name"]
        print(f"\nRunning benchmark: {bname} ({bid}) ...")

        # 1. Spid3r VM
        vm_time, vm_out = measure([spid3r_bin, "run", "--engine", "vm", b["spid3r"]], runs)
        # 2. Spid3r Bytecode Exec (.matrixc directly)
        exec_time, exec_out = measure([spid3r_bin, "exec", compiled_files[bid]], runs)
        # 3. Spid3r Tree Interpreter
        tree_time, tree_out = measure([spid3r_bin, "run", "--engine", "tree", b["spid3r"]], runs)
        # 4. Python 3
        py_time, py_out = measure([python_bin, b["python"]], runs)
        # 5. PHP
        php_time, php_out = (None, None)
        if php_bin:
            php_time, php_out = measure([php_bin, b["php"]], runs)

        # Verify checksums
        expected = b["expected"]
        print(f"  Checksum: expected={expected}")
        print(f"    Spid3r VM:     {vm_out} ({'OK' if vm_out == expected else 'MISMATCH'}) -> {vm_time*1000:.1f} ms")
        print(f"    Spid3r Exec:   {exec_out} ({'OK' if exec_out == expected else 'MISMATCH'}) -> {exec_time*1000:.1f} ms")
        print(f"    Spid3r Tree:   {tree_out} ({'OK' if tree_out == expected else 'MISMATCH'}) -> {tree_time*1000:.1f} ms")
        print(f"    Python 3:      {py_out} ({'OK' if py_out == expected else 'MISMATCH'}) -> {py_time*1000:.1f} ms")
        if php_bin:
            print(f"    PHP 8:         {php_out} ({'OK' if php_out == expected else 'MISMATCH'}) -> {php_time*1000:.1f} ms")

        results.append({
            "id": bid,
            "name": bname,
            "desc": b["desc"],
            "expected": expected,
            "spid3r_vm_ms": vm_time * 1000.0,
            "spid3r_exec_ms": exec_time * 1000.0,
            "spid3r_tree_ms": tree_time * 1000.0,
            "python_ms": py_time * 1000.0,
            "php_ms": (php_time * 1000.0) if php_time else None,
        })

    # Save JSON data
    report_data = {
        "timestamp": time.strftime("%Y-%m-%d %H:%M:%S"),
        "runs": runs,
        "versions": {
            "spid3r": spid3r_ver,
            "python": py_ver,
            "php": php_ver_raw,
        },
        "benchmarks": results,
    }
    with open("benchmarks/results.json", "w") as f:
        json.dump(report_data, f, indent=2)

    # Update official interactive HTML report in docs/html/benchmarks.html
    update_benchmarks_html(report_data)
    print(f"\n[OK] Benchmark run complete! Results written to docs/html/benchmarks.html and benchmarks/results.json")

def update_benchmarks_html(data, target_path=None):
    if target_path is None:
        target_path = Path(__file__).resolve().parent.parent / "docs" / "html" / "benchmarks.html"
    else:
        target_path = Path(target_path)

    if not target_path.exists():
        print(f"[WARN] {target_path} not found. Skipping HTML update.")
        return

    content = target_path.read_text(encoding="utf-8")

    # Embed or update the JSON data in <script id="benchmark-data" type="application/json">
    json_str = json.dumps(data, indent=2)
    script_tag = f'<script id="benchmark-data" type="application/json">\n{json_str}\n</script>'
    if '<script id="benchmark-data"' in content:
        content = re.sub(r'<script id="benchmark-data"[^>]*>.*?</script>', script_tag, content, flags=re.DOTALL)
    else:
        content = content.replace('</body>', f'{script_tag}\n</body>')

    # Update the host environment spec card with the *actual* host and the
    # actual best-of count (the old code wrote a hardcoded "Microsoft Windows 11
    # x64" string regardless of where it ran).
    runs = data.get("runs", 3)
    host = f"{platform.system()} {platform.release()} {platform.machine()}".strip()
    content = re.sub(
        r'(<div class="spec-card">\s*<h4>Host Environment</h4>\s*<div class="val">)[^<]*(</div>)',
        rf'\g<1>{host} · Best-of-{runs} runs\g<2>',
        content
    )

    # Clean up benchmark compiled bytecode
    bench_dir = target_path.parent.parent.parent / "benchmarks"
    for mc in bench_dir.glob("*.matrixc"):
        try:
            mc.unlink()
        except OSError:
            pass

    # A stray benchmarks.html at the repository root used to be deleted here.
    # That is someone else's file: report it and leave it alone.
    root_file = target_path.parent.parent.parent / "benchmarks.html"
    if root_file.exists():
        print(f"[WARN] {root_file} exists at the repository root; leaving it untouched.")

    target_path.write_text(content, encoding="utf-8")

if __name__ == "__main__":
    main()
