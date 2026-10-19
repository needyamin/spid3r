#!/usr/bin/env python3
"""
tools/bench_compare.py - Multi-language benchmark runner and comparative analyzer.
Compares Spid3r (Tree-walk, Bytecode VM, and Compiled .matrixc) against Python 3 and PHP.
Generates both terminal output and a beautiful interactive benchmarks.html report.
"""

import sys
import os
import time
import subprocess
import json
import shutil
from pathlib import Path

# Ensure PHP and Python are discoverable from updated Windows PATH if needed
user_path = os.environ.get("Path", "")
# Append potential PHP directory if winget installed it
php_candidate = Path(r"C:\Program Files\PHP\v8.3")
if php_candidate.exists() and str(php_candidate) not in user_path:
    os.environ["Path"] = str(php_candidate) + ";" + user_path

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

    # Generate HTML report
    generate_html_report(report_data, "benchmarks.html")
    docs_html = Path("docs/html/benchmarks.html")
    if docs_html.parent.exists():
        generate_html_report(report_data, str(docs_html))
    print(f"\n[OK] Benchmark run complete! Results written to benchmarks.html, docs/html/benchmarks.html, and benchmarks/results.json")

def generate_html_report(data, out_path):
    benchmarks = data["benchmarks"]
    rows = []
    
    # Calculate geometric means / speedups
    for b in benchmarks:
        svm = b["spid3r_vm_ms"]
        sexec = b["spid3r_exec_ms"]
        stree = b["spid3r_tree_ms"]
        py = b["python_ms"]
        php = b["php_ms"]

        # Comparison ratio: how many times faster/slower relative to Python (py / time)
        # e.g., > 1.0 means faster than Python, < 1.0 means slower
        speedup_vm_vs_py = py / svm if svm > 0 else 0
        speedup_php_vs_py = (py / php) if php and php > 0 else 0

        rows.append(f"""
        <tr>
          <td><strong>{b['name']}</strong><br><small class="desc">{b['desc']}</small></td>
          <td class="num font-mono"><strong>{svm:.1f} ms</strong></td>
          <td class="num font-mono">{sexec:.1f} ms</td>
          <td class="num font-mono">{stree:.1f} ms</td>
          <td class="num font-mono">{py:.1f} ms</td>
          <td class="num font-mono">{f"{php:.1f} ms" if php else "N/A"}</td>
          <td class="num font-mono highlight">{f"{speedup_vm_vs_py:.2f}x" if speedup_vm_vs_py >= 1 else f"{speedup_vm_vs_py:.2f}x"}</td>
          <td class="num font-mono">{f"{speedup_php_vs_py:.2f}x" if php else "N/A"}</td>
        </tr>
        """)

    table_rows = "\n".join(rows)

    html = f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Spid3r vs Python vs PHP Benchmark Report</title>
<style>
  :root {{
    --bg: #0f141c;
    --card: #18202c;
    --card-border: #283548;
    --text: #e2e8f0;
    --text-muted: #94a3b8;
    --accent: #38bdf8;
    --accent-glow: rgba(56, 189, 248, 0.15);
    --green: #4ade80;
    --amber: #fbbf24;
    --table-stripe: #131b26;
  }}
  * {{ box-sizing: border-box; margin: 0; padding: 0; }}
  body {{
    background: var(--bg);
    color: var(--text);
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
    line-height: 1.6;
    padding: 2.5rem 1.5rem;
  }}
  .container {{
    max-width: 1100px;
    margin: 0 auto;
  }}
  header {{
    margin-bottom: 2.5rem;
    padding-bottom: 1.5rem;
    border-bottom: 1px solid var(--card-border);
  }}
  h1 {{
    font-size: 2.2rem;
    font-weight: 700;
    color: #fff;
    margin-bottom: 0.5rem;
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }}
  h1 .badge {{
    font-size: 0.85rem;
    padding: 0.2rem 0.6rem;
    border-radius: 999px;
    background: var(--accent-glow);
    color: var(--accent);
    border: 1px solid var(--accent);
    vertical-align: middle;
  }}
  .subtitle {{
    color: var(--text-muted);
    font-size: 1.05rem;
  }}
  .meta-grid {{
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
    gap: 1rem;
    margin: 1.5rem 0 2.5rem 0;
  }}
  .meta-card {{
    background: var(--card);
    border: 1px solid var(--card-border);
    border-radius: 10px;
    padding: 1.25rem;
  }}
  .meta-card h3 {{
    font-size: 0.9rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
    margin-bottom: 0.5rem;
  }}
  .meta-card .val {{
    font-size: 1.15rem;
    font-weight: 600;
    color: #fff;
  }}
  .table-wrap {{
    background: var(--card);
    border: 1px solid var(--card-border);
    border-radius: 12px;
    overflow-x: auto;
    box-shadow: 0 10px 25px rgba(0,0,0,0.3);
  }}
  table {{
    width: 100%;
    border-collapse: collapse;
    text-align: left;
    font-size: 0.95rem;
  }}
  th {{
    background: #1e293b;
    color: #cbd5e1;
    font-weight: 600;
    padding: 1rem 1.2rem;
    border-bottom: 2px solid var(--card-border);
    white-space: nowrap;
  }}
  td {{
    padding: 1rem 1.2rem;
    border-bottom: 1px solid var(--card-border);
    vertical-align: middle;
  }}
  tr:nth-child(even) {{ background: var(--table-stripe); }}
  tr:hover {{ background: rgba(56, 189, 248, 0.06); }}
  .num {{ text-align: right; }}
  .font-mono {{ font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; }}
  .desc {{ color: var(--text-muted); font-size: 0.8rem; display: block; margin-top: 0.2rem; }}
  .highlight {{ color: var(--green); font-weight: 700; }}
  .takeaway-grid {{
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
    gap: 1.25rem;
    margin-top: 2.5rem;
  }}
  .takeaway-card {{
    background: var(--card);
    border: 1px solid var(--card-border);
    border-radius: 10px;
    padding: 1.5rem;
  }}
  .takeaway-card h3 {{
    color: var(--accent);
    margin-bottom: 0.6rem;
    font-size: 1.1rem;
  }}
  .takeaway-card p {{
    color: var(--text-muted);
    font-size: 0.92rem;
  }}
  footer {{
    margin-top: 3.5rem;
    text-align: center;
    color: var(--text-muted);
    font-size: 0.85rem;
    border-top: 1px solid var(--card-border);
    padding-top: 1.5rem;
  }}
</style>
</head>
<body>
<div class="container">
  <header>
    <h1>Spid3r vs Python vs PHP <span class="badge">Benchmark Suite</span></h1>
    <p class="subtitle">Fair, identical-workload comparison across all language runtimes with checksum validation.</p>
  </header>

  <div class="meta-grid">
    <div class="meta-card">
      <h3>Spid3r Toolchain</h3>
      <div class="val">{data['versions']['spid3r']}</div>
      <small style="color:var(--text-muted)">Rust-powered VM & Bytecode Compiler</small>
    </div>
    <div class="meta-card">
      <h3>Python Version</h3>
      <div class="val">{data['versions']['python']}</div>
      <small style="color:var(--text-muted)">CPython 64-bit</small>
    </div>
    <div class="meta-card">
      <h3>PHP Version</h3>
      <div class="val">{data['versions']['php']}</div>
      <small style="color:var(--text-muted)">Zend Engine 64-bit CLI</small>
    </div>
  </div>

  <div class="table-wrap">
    <table>
      <thead>
        <tr>
          <th>Benchmark Workload</th>
          <th class="num">Spid3r (VM)</th>
          <th class="num">Spid3r (.matrixc)</th>
          <th class="num">Spid3r (Tree)</th>
          <th class="num">Python 3</th>
          <th class="num">PHP 8</th>
          <th class="num">Spid3r vs Py</th>
          <th class="num">PHP vs Py</th>
        </tr>
      </thead>
      <tbody>
        {table_rows}
      </tbody>
    </table>
  </div>

  <div class="takeaway-grid">
    <div class="takeaway-card">
      <h3>⚡ Spid3r VM vs Python 3</h3>
      <p>Spid3r's bytecode VM with upvalues and stack-based operand caching executes object method dispatch and tight arithmetic with competitive efficiency to standard CPython, while compiling instantly without JIT startup pauses.</p>
    </div>
    <div class="takeaway-card">
      <h3>📦 Bytecode (.matrixc) Precompilation</h3>
      <p>Directly executing standalone <code>.matrixc</code> bytecode eliminates AST parsing and semantic analysis overhead, delivering faster cold-start execution times.</p>
    </div>
    <div class="takeaway-card">
      <h3>🐘 PHP 8 Zend Engine</h3>
      <p>PHP 8's highly tuned opcode engine performs very well in array churn and string manipulations, providing a strong reference baseline for dynamic language optimization.</p>
    </div>
  </div>

  <footer>
    Generated on {data['timestamp']} · Best-of-{data['runs']} runs · Self-contained report
  </footer>
</div>
</body>
</html>
"""
    with open(out_path, "w", encoding="utf-8") as f:
        f.write(html)

if __name__ == "__main__":
    main()
