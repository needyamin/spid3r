#!/usr/bin/env python3
"""Self-host semantic fuzz parity (roadmap 3.3, SH-5).

Feeds random and mutated inputs to BOTH checkers — the stage-0 oracle
(`spid3r check`) and the Spid3r-written port (`spid3r run selfhost/main.matrix
check <f>`) — and demands agreement on the first diagnostic:

  * oracle checked cleanly -> both must check cleanly;
  * oracle produced a semantic diagnostic -> first line + `-->` span line
    must match exactly;
  * oracle failed in the lex/parse phases -> out of this fuzz's domain
    (those harnesses cover them), skipped.

ORACLE INSTABILITY GUARD: the Rust checker walks two HashMaps when
diagnosing multi-violation programs (abstract contracts, interface
conformance), and Rust HashMap iteration order is randomized per process.
An input whose oracle output differs between two oracle runs is skipped as
oracle-unstable — recorded, not hidden.

All inputs are valid UTF-8 by construction. The RNG is deliberately seeded
for reproducibility (the gate is a rerunnable experiment whose seed is
recorded in the ledger).

Usage: python tools/fuzz_semantic_parity.py [--spid3r BIN] [--engine tree]
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
    + ["let", "const", "fn", "class", "interface", "enum", "match", "self",
       "new", "super", "public", "private", "abstract", "static", "extends",
       "implements", "return", "try", "throw", "spawn", "await", "async", "as"]
    + ["int", "float", "string", "bool", "null", "list<int>", "map<string, int>",
       "T?", "int | string", "some", "ok", "err"]
    + ["é", "🚀", "日", "ß", "\uffff", "", " "]
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
    oracle_unstable = 0
    out_of_domain = 0
    failed = []
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

            o1 = subprocess.run(
                [binary, "check", str(path)],
                capture_output=True, cwd=str(ROOT), stdin=subprocess.DEVNULL, timeout=30,
            )
            o2 = subprocess.run(
                [binary, "check", str(path)],
                capture_output=True, cwd=str(ROOT), stdin=subprocess.DEVNULL, timeout=30,
            )
            if first_two(o1.stderr) != first_two(o2.stderr) or o1.returncode != o2.returncode:
                oracle_unstable += 1
                continue

            s = subprocess.run(
                [binary, "run", "--engine", args.engine, "selfhost/main.matrix", "check", str(path)],
                capture_output=True, cwd=str(ROOT), stdin=subprocess.DEVNULL, timeout=30,
            )
            if o1.returncode == 0:
                if s.returncode == 0:
                    matched += 1
                else:
                    failed.append((i, "selfhost-errored", "", s.stderr[:120]))
            else:
                oerr = o1.stderr.decode("utf-8", "replace")
                if oerr.startswith("error[semantic]:"):
                    if s.returncode != 0 and first_two(o1.stderr) == first_two(s.stderr):
                        matched += 1
                    else:
                        failed.append((i, "diagnostic", oerr[:120], s.stderr[:120]))
                else:
                    out_of_domain += 1

    print(
        f"fuzz: {args.cases} cases, seed {args.seed}: "
        f"{matched} matched, {oracle_unstable} oracle-unstable (skipped), "
        f"{out_of_domain} out-of-domain (lex/parse), {len(failed)} FAILED"
    )
    for i, kind, o, s in failed[:10]:
        print(f"  case {i} [{kind}]")
        print(f"    oracle:   {o!r}")
        print(f"    selfhost: {s!r}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
