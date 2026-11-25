#!/usr/bin/env python3
"""Enforce AGENTS.md §3 unsafe-boundary and // SAFETY: comment invariants."""

import glob
import os
import re
import sys

ALLOWED_UNSAFE_FILES = {
    "src/native/ir.rs",
    "src/native/link.rs",
    "src/native/llvm.rs",
    "src/native/rt.rs",
    "src/repl/mod.rs",
    "src/runtime_info/mod.rs",
    "src/stdlib/db.rs",
    "src/stdlib/entropy.rs",
    "src/stdlib/eventloop.rs",
    "src/stdlib/file.rs",
    "src/stdlib/net.rs",
    "src/stdlib/shutdown.rs",
    "src/value/collections/seq.rs",
    "src/value/mod.rs",
}


def main() -> int:
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    os.chdir(root)
    failures = []
    total_unsafe = 0
    total_safety = 0
    checked_files = 0

    for path in sorted(glob.glob("src/**/*.rs", recursive=True)):
        norm = path.replace("\\", "/")
        with open(path, encoding="utf-8") as fh:
            txt = fh.read()
        u = len(re.findall(r"\bunsafe\s*\{", txt))
        if u == 0:
            continue
        checked_files += 1
        s = len(re.findall(r"//\s*SAFETY:", txt))
        total_unsafe += u
        total_safety += s
        if norm not in ALLOWED_UNSAFE_FILES:
            failures.append(f"{norm}: unsafe block outside AGENTS.md §3 allowlist ({u} blocks)")
        if s < u:
            failures.append(f"{norm}: {u} unsafe blocks but only {s} // SAFETY: comments")

    if failures:
        for msg in failures:
            print(f"FAIL: {msg}", file=sys.stderr)
        return 1

    print(
        f"PASS: unsafe/SAFETY gate ({checked_files} allowlisted files, "
        f"{total_unsafe} unsafe blocks, {total_safety} // SAFETY: comments)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
