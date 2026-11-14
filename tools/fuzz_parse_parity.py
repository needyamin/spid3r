#!/usr/bin/env python3
"""Self-host parser fuzz parity (roadmap 2.3, SH-5).

Feeds random and mutated inputs to BOTH parsers — the stage-0 oracle
(`spid3r parse <f> --dump-ast-json`) and the Spid3r-written port
(`spid3r run selfhost/main.matrix parse <f>`) — and demands agreement:

  * oracle parsed cleanly          -> AST JSON byte-identical;
  * oracle rejected (parse or lexer diagnostic) -> first line + `-->` span
    line identical;
  * oracle failed some other way   -> out of the fuzz domain, skipped.

Inputs are corpus files, character-level mutations, and random strings over
a parser-hot alphabet. All valid UTF-8 (the CLI-reachable domain on both
sides). Phase 2 closed the Phase-1 interp divergence (the injected
hole_is_expression), so there are NO divergence buckets: every difference
is a failure.

The RNG is deliberately seeded for reproducibility — the gate is a
rerunnable experiment whose seed is recorded in the ledger.

Usage: python tools/fuzz_parse_parity.py [--spid3r BIN] [--engine tree]
       [--cases N] [--seed S]
"""

import argparse
import random
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPUS_DIRS = ["examples", "engine_matrix", "edge"]

HOT = (
    list(" \t\r\n{}[]()\"'\\")
    + list("0123456789_")
    + list("abcdefxXoObBeEnn+-*/%=<>!&|^:;,.")
    + ["r", "r#", "0x", "0b", "0o", "\\u", "\\u{", "\\n", "\\t", "\\\"", "\\\\",
       "/*", "*/", "//", "#!", "#{", "=>", "?.", "??", "<<", ">>", "..", "?", ":"]
    + ["fn", "let", "match", "=>", "else", "elif", "class", "enum", "interface",
       "new", "super", "spawn", "await", "yield", "as", "and", "or", "not"]
    + ["é", "🚀", "日", "ß", "\uffff", "\ufffd", "", " "]
)


def load_seeds() -> list:
    seeds = []
    for d in CORPUS_DIRS:
        for f in sorted((ROOT / "selfhost" / "corpus" / d).glob("*.matrix")):
            seeds.append(f.read_text(encoding="utf-8"))
    return seeds


def mutate(rng: random.Random, text: str) -> str:
    s = list(text)
    for _ in range(rng.randint(1, 6)):
        if not s:
            break
        op = rng.randrange(3)
        i = rng.randrange(len(s))
        if op == 0:
            s[i] = rng.choice(HOT)
        elif op == 1:
            s.insert(i, rng.choice(HOT))
        else:
            del s[i]
    return "".join(s)


def random_case(rng: random.Random) -> str:
    n = rng.randint(1, 80)
    return "".join(rng.choice(HOT) for _ in range(n))


def first_two(b: bytes) -> str:
    text = b.decode("utf-8", "replace")
    lines = text.splitlines()
    return "\n".join(lines[:2])


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--spid3r", default="target/release/spid3r")
    ap.add_argument("--engine", default="tree")
    ap.add_argument("--cases", type=int, default=1000)
    ap.add_argument("--seed", type=int, default=20261114)
    args = ap.parse_args()

    binary = args.spid3r
    if not Path(binary).exists() and not (ROOT / binary).exists() \
            and not Path(binary + ".exe").exists():
        print(f"binary not found: {binary}", file=sys.stderr)
        return 2
    if not Path(binary).exists() and Path(binary + ".exe").exists():
        binary = binary + ".exe"

    rng = random.Random(args.seed)
    seeds = load_seeds()
    if not seeds:
        print("no corpus seeds found", file=sys.stderr)
        return 2

    matched = 0
    failed = []
    skipped = 0
    with tempfile.TemporaryDirectory() as td:
        path = Path(td) / "case.matrix"
        for i in range(args.cases):
            if i % 3 == 0:
                text = rng.choice(seeds)
            elif i % 3 == 1:
                text = mutate(rng, rng.choice(seeds))
            else:
                text = random_case(rng)
            path.write_text(text, encoding="utf-8", newline="")

            o = subprocess.run(
                [binary, "parse", str(path), "--dump-ast-json"],
                capture_output=True, cwd=str(ROOT), stdin=subprocess.DEVNULL, timeout=30,
            )
            s = subprocess.run(
                [binary, "run", "--engine", args.engine, "selfhost/main.matrix", "parse", str(path)],
                capture_output=True, cwd=str(ROOT), stdin=subprocess.DEVNULL, timeout=30,
            )
            if o.returncode == 0:
                if s.returncode == 0 and o.stdout == s.stdout:
                    matched += 1
                else:
                    failed.append((i, "ast", o.stdout[:120], s.stdout[:120]))
            else:
                oerr = o.stderr.decode("utf-8", "replace")
                if oerr.startswith("error[") and (
                    oerr.startswith("error[parser]:") or oerr.startswith("error[lexer]:")
                ):
                    if s.returncode != 0 and first_two(o.stderr) == first_two(s.stderr):
                        matched += 1
                    else:
                        failed.append((i, "diagnostic", oerr[:120], s.stderr[:120]))
                else:
                    skipped += 1

    print(
        f"fuzz: {args.cases} cases, seed {args.seed}: "
        f"{matched} matched, {skipped} out-of-domain, {len(failed)} FAILED"
    )
    for i, kind, o, s in failed[:10]:
        print(f"  case {i} [{kind}]")
        print(f"    oracle:   {o!r}")
        print(f"    selfhost: {s!r}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
