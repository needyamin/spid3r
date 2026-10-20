"""Re-verification probe for the 2026-10-20 audit findings (2026-10-21).

Runs small Spid3r programs against the current binary and prints one line per
check: `OK` when the finding's bad behaviour is gone, `OPEN` when it still
reproduces, plus the observed output. Used to update assessment/ with facts
instead of assumptions.

Usage: python audit_notes/probes/verify_2026-10-21.py [--bin PATH]
"""

import os
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.environ.get("SPID3R_BIN") or os.path.join(ROOT, "target", "debug", "spid3r.exe")
TIMEOUT = 30.0

# (id, title, source, expectation)
#   expectation: "clean"  -> exit 1 with a diagnostic and no panic
#                "ok"     -> exit 0
#                "open"   -> documents a finding that is expected to still reproduce
CASES = [
    # ---------------------------------------------------------------- fixed
    ("A-1", 'bytes.from_hex("a<U+26A1>")',
     'fn main() {\n    let b = bytes().from_hex("a\u26a1");\n    println(b.len());\n    return 0;\n}\n',
     "clean"),
    ("A-1b", 'bytes.from_hex("414243")',
     'fn main() {\n    let b = bytes().from_hex("414243");\n    println(b.len());\n    return 0;\n}\n',
     "ok"),
    ("A-2", "regex nesting 600 groups",
     'fn main() {\n    let p = "' + "(" * 600 + "a" + ")" * 600 + '";\n'
     '    let r = regex_find(p, "a");\n    println(r);\n    return 0;\n}\n',
     "clean"),
    ("A-2b", "regex normal match",
     'fn main() {\n    println(regex_find("[a-z]+", "123 orange 456"));\n    return 0;\n}\n',
     "ok"),
    ("A-6", "sort a list containing NaN",
     'fn main() {\n    let l = [nan(), 1.0, 2.0];\n    l.sort();\n    println(l.len());\n    return 0;\n}\n',
     "ok"),
    ("A-6b", "sort mixed huge int/float",
     'fn main() {\n    let l = [9007199254740993, 1.5, 2, 0.25];\n    l.sort();\n    println(l);\n    return 0;\n}\n',
     "ok"),
    ("A-7", "self-referential list contains/remove/slice",
     'fn main() {\n    let l = list();\n    l.push(l);\n    println(l.contains(l));\n'
     '    println(l.slice(0, 1).len());\n    l.remove(l);\n    println(l.len());\n    return 0;\n}\n',
     "ok"),
    ("A-4", "gc_collect while a task runs",
     'async fn wait(t) { return await t; }\n'
     'fn main() {\n    let t = spawn fn() { let i = 0; while (i < 200000) { i = i + 1; } return 1; };\n'
     '    sleep_ms(20);\n    try { gc_collect(); println("swept"); } catch(e) { println("refused"); }\n'
     '    let v = wait(t);\n    return 0;\n}\n',
     "ok"),
    ("A-9", "thrown map across await",
     'async fn wait(t) { return await t; }\n'
     'fn main() {\n    let t = spawn fn() {\n        let o = map();\n        o.set("code", 7);\n        throw o;\n    };\n'
     '    try { let v = wait(t); println("no-throw"); } catch(e) { println(type_of(e)); println(e["code"]); }\n'
     '    return 0;\n}\n',
     "ok"),
    ("A-10a", "index-assign target checks its index",
     'fn main() {\n    let a = [1];\n    a[ghost()] = 1;\n    return 0;\n}\n',
     "clean"),
    ("A-10b", "fn main arity validated",
     'fn main(x) {\n    return 0;\n}\n',
     "clean"),
    ("A-10c", "function signatures compared",
     'fn main() {\n    let f = fn(x: int) -> int { return x; };\n'
     '    let g = fn(x: int) -> string { return "s"; };\n    f = g;\n    return 0;\n}\n',
     "clean"),

    # ------------------------------------------------------ expected open
    ("A-14", "deep_clone of a cyclic value aliases the original",
     'fn main() {\n    let a = list();\n    a.push(a);\n    let b = a.clone();\n'
     '    b[0].push("x");\n    println(a.len());\n    println(b.len());\n    return 0;\n}\n',
     "open"),
    ("A-15", "regex restarts its NFA per input index (O(n^2))",
     'fn main() {\n    let s = "";\n    let i = 0;\n    while (i < 20000) { s = s + "a"; i = i + 1; }\n'
     '    let t0 = now();\n    let r = regex_find("a+b", s);\n'
     '    println(now() - t0);\n    return 0;\n}\n',
     "open"),
    ("A-17", "deeply nested stack dropped at scope exit",
     'fn main() {\n    let d = 0;\n    let s = stack();\n'
     '    while (d < 200000) { let n = stack(); n.push(s); s = n; d = d + 1; }\n'
     '    println("built");\n    return 0;\n}\n',
     "open"),
    ("low-csv-bom", "CSV header keeps the UTF-8 BOM",
     'fn main() {\n    let t = parse_csv("\ufeffname,age\\nada,36\\n", null).unwrap();\n'
     '    let h = t["headers"][0];\n    println(h.len());\n    println(h);\n    return 0;\n}\n',
     "open"),
    ("low-regex-nl", "negated class does not match newline",
     'fn main() {\n    println(regex_match("[^a]", "\\n"));\n    return 0;\n}\n',
     "open"),
]


def run(src, timeout=TIMEOUT):
    fd, path = tempfile.mkstemp(suffix=".matrix")
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="") as f:
            f.write(src)
        p = subprocess.Popen(
            [BIN, "run", path],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            out, err = p.communicate(timeout=timeout)
            return p.returncode, out.decode("utf-8", "replace").strip(), err.decode("utf-8", "replace").strip()
        except subprocess.TimeoutExpired:
            p.kill()
            p.communicate()
            return "TIMEOUT", "", ""
    finally:
        os.unlink(path)


def main():
    only = [a for a in sys.argv[1:] if not a.startswith("--")]
    rows = []
    for cid, title, src, expect in CASES:
        if only and cid not in only:
            continue
        code, out, err = run(src)
        flat_out = " / ".join(out.splitlines())
        flat_err = err.splitlines()[0] if err else ""
        if code == "TIMEOUT":
            verdict = "OPEN" if expect == "open" else "TIMEOUT"
        elif expect == "clean":
            verdict = "OK" if code == 1 and "panicked" not in err else "FAIL"
        elif expect == "ok":
            verdict = "OK" if code == 0 and "panicked" not in err else "FAIL"
        else:  # "open" — record what it actually does
            verdict = "OPEN" if code != 0 or out else "CHANGED?"
        rows.append((cid, title, verdict, code, flat_out[:70], flat_err[:70]))
        print(f"{cid:12s} {verdict:8s} exit={str(code):7s} out={flat_out[:60]!r} err={flat_err[:60]!r}")
    print()
    bad = [r for r in rows if r[2] in ("FAIL", "TIMEOUT", "CHANGED?")]
    print(f"{len(rows)} checks, {len(bad)} unexpected")
    for r in bad:
        print("  unexpected:", r[0], r[1], r[2], r[3])


if __name__ == "__main__":
    main()
