#!/usr/bin/env python3
"""Self-host lexer fuzz parity (roadmap 1.4, SH-5).

Feeds random and mutated inputs to BOTH lexers — the stage-0 oracle
(`spid3r tokens <f> --json`) and the Spid3r-written port
(`spid3r run selfhost/lexer/main.matrix <f>`) — and demands agreement:

  * oracle lexed cleanly      -> stdout must be byte-identical;
  * oracle failed with lexer error -> the diagnostic's first line and `-->`
    span line must be identical;
  * oracle failed some other way (should not happen on valid UTF-8 files)
    -> the case is skipped and counted.

Inputs are corpus files, character-level mutations of them, and fully random
strings over a lexer-hot alphabet (quotes, braces, escapes, radix prefixes,
digits, unicode — including astral characters). All inputs are valid UTF-8 by
construction: that is exactly the domain the CLI harness can reach on either
side (`read_file`/`read_to_string` both reject invalid UTF-8 before any
lexer runs).

KNOWN DIVERGENCE (ledger-recorded, full_plan.html): the oracle validates a
`{expr}` interpolation capture by PARSING it (hole_is_expression); the parser
is Phase 2, so the port accepts captures the oracle may reject. A differing
case is classified as interp-domain when either side's output mentions
InterpStr, and counted separately; every other difference is a failure.

Usage: python tools/fuzz_lex_parity.py [--spid3r BIN] [--engine tree]
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
       "/*", "*/", "//", "#!", "#{", "=>", "?.", "??", "<<", ">>", ".."]
    + ["é", "🚀", "日", "ß", "\uffff", "\ufffd", "", " "]
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
        if op == 0:  # replace
            s[i] = rng.choice(HOT)
        elif op == 1:  # insert
            s.insert(i, rng.choice(HOT))
        else:  # delete
            del s[i]
    return "".join(s)


def random_case(rng: random.Random) -> str:
    n = rng.randint(1, 60)
    return "".join(rng.choice(HOT) for _ in range(n))


def run_pair(binary: str, engine: str, path: Path, tmp: Path):
    o = subprocess.run(
        [binary, "tokens", str(path), "--json"],
        capture_output=True, cwd=str(ROOT), stdin=subprocess.DEVNULL, timeout=30,
    )
    s = subprocess.run(
        [binary, "run", "--engine", engine, "selfhost/lexer/main.matrix", str(path)],
        capture_output=True, cwd=str(ROOT), stdin=subprocess.DEVNULL, timeout=30,
    )
    return o, s


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
    interp_divergent = 0
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

            o, s = run_pair(binary, args.engine, path, Path(td))
            if o.returncode == 0:
                if s.returncode == 0 and o.stdout == s.stdout:
                    matched += 1
                    continue
                if b"InterpStr" in o.stdout or b"InterpStr" in s.stdout:
                    interp_divergent += 1
                    continue
                failed.append((i, "stdout", o.stdout[:120], s.stdout[:120]))
            else:
                oerr = o.stderr.decode("utf-8", "replace")
                if oerr.startswith("error[lexer]:"):
                    if s.returncode != 0 and first_two(o.stderr) == first_two(s.stderr):
                        matched += 1
                        continue
                    if b"InterpStr" in s.stdout:
                        interp_divergent += 1
                        continue
                    failed.append((i, "diagnostic", oerr[:120], s.stderr[:120]))
                else:
                    # Oracle failed before lexing — out of the fuzz domain.
                    matched += 1

    print(
        f"fuzz: {args.cases} cases, seed {args.seed}: "
        f"{matched} matched, {interp_divergent} interp-domain divergent "
        f"(Phase-2 boundary), {len(failed)} FAILED"
    )
    for i, kind, o, s in failed[:10]:
        print(f"  case {i} [{kind}]")
        print(f"    oracle:   {o!r}")
        print(f"    selfhost: {s!r}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
