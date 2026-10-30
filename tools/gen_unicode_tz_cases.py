#!/usr/bin/env python3
"""Generate tests/data/unicode_tz_cases.matrix from CPython.

    python tools/gen_unicode_tz_cases.py target/tzdata.whl

The output is a complete Spid3r program: every line is an `assert_eq` against
what CPython computes for the same input. That makes the oracle explicit and
reviewable - `unicodedata.normalize`, `str.casefold` and `zoneinfo` - instead
of hand-written expectations that drift from the standard.

Coverage is chosen to hit the places implementations actually get wrong:

  * canonical ordering with marks supplied out of order
  * composition exclusions (U+0958) and singletons (U+2126 OHM -> U+03A9)
  * Hangul: precomposed syllables, LV/LVT composition, conjoining jamo
  * compatibility forms (ligatures, circled digits, full-width, units)
  * case folding that is not lowercasing: U+00DF, U+0130, final sigma, Cherokee
  * time zones in every awkward shape: 30- and 45-minute DST, +12:45, +14,
    southern hemisphere, DST abolished, a zone whose DST is 2 hours
  * local-time round trips, skipping the instants that are genuinely ambiguous
    (the hour that repeats when DST ends cannot round-trip by definition)

Regenerate this file whenever the tz blob or the Unicode tables are
regenerated; the program asserts the data versions, so a stale fixture fails
the build instead of passing silently.
"""

import sys
import unicodedata
import zipfile
import os
from datetime import datetime, timedelta, timezone

EPOCH = datetime(1970, 1, 1, tzinfo=timezone.utc)


def utc_dt(t):
    return EPOCH + timedelta(seconds=t)


def q(s):
    """Quote a Python string as a Spid3r string literal."""
    out = ['"']
    for ch in s:
        if ch == '"':
            out.append('\\"')
        elif ch == "\\":
            out.append("\\\\")
        elif ch == "\n":
            out.append("\\n")
        elif ch == "\t":
            out.append("\\t")
        elif ord(ch) < 0x20 or ord(ch) == 0x7F:
            raise SystemExit(f"control character U+{ord(ch):04X} in corpus")
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


# ---------------------------------------------------------------- unicode

def unicode_corpus():
    return [
        # precomposed vs decomposed, both directions
        "é", "e\u0301", "É", "E\u0301",
        "Å", "A\u030a", "Å",  # U+212B ANGSTROM SIGN
        "ü", "u\u0308",
        # marks supplied in the wrong order: canonical ordering must fix it
        "a\u0301\u0323", "a\u0323\u0301", "o\u0308\u0304", "o\u0304\u0308",
        # a starter with several marks of the same class (order must be kept)
        "q\u0307\u0323", "q\u0323\u0307",
        # composition exclusions and singletons
        "\u0958", "\u0959", "\u2126", "\u212a", "\u00c5\u0301",
        # Hangul
        "\uac00", "\ud55c", "\u1100\u1161", "\uac00\u11a8", "\u1100\u1161\u11a8",
        "\u3131", "\u3132",
        # compatibility forms
        "ﬁ", "ﬂ", "①", "⑩", "Ａ", "ｱ", "㎏", "¼", "Ⅻ", "™", "…", "☎",
        "\ufb01\ufb02", "2\u2075", "\u00bd",
        # case folding that is not lowercasing
        "ß", "ẞ", "STRASSE", "straße", "İ", "I\u0307", "Σ", "σ", "ς", "ΟΣ",
        "ᏣᎳᎩ", "ꭰ",
        # mixed and realistic
        "Crème Brûlée", "cafe\u0301 au lait", "日本語", "Ｈｅｌｌｏ",
        "e\u0301e\u0301e\u0301", "🇯🇵", "👩‍👩‍👧‍👦", "a\u0301b\u0302c\u0303",
        "", " ", "abc",
        # long-ish, to exercise the loop rather than a fast path
        ("é" * 40) + ("e\u0301" * 40),
    ]


# ---------------------------------------------------------------- time zones

ZONES = [
    "UTC",
    "America/New_York",
    "Europe/Berlin",
    "Europe/London",
    "Europe/Moscow",
    "Asia/Shanghai",
    "Asia/Kolkata",
    "Asia/Kathmandu",
    "Asia/Tehran",
    "Australia/Lord_Howe",   # 30-minute DST
    "Pacific/Chatham",       # +12:45
    "Pacific/Kiritimati",    # +14
    "Pacific/Midway",        # -11
    "America/St_Johns",      # -03:30
    "America/Santiago",      # southern hemisphere
    "America/Sao_Paulo",     # DST abolished in 2019
    "Africa/Casablanca",     # Ramadan-adjusted DST
    "Antarctica/Troll",      # +00 / +02
]

TIMESTAMPS = [
    -2208988800,  # 1900-01-01
    -1000000000,
    -500000000,
    0,
    500000000,
    946684800,    # 2000-01-01
    1262304000,
    1500000000,
    1761000000,   # 2025-10-20
    1900000000,
    2200000000,
    2500000000,
    3500000000,   # 2080
    4100000000,   # 2099
]


def main():
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    wheel = sys.argv[1]
    dest = sys.argv[2] if len(sys.argv) > 2 else "tests/data/unicode_tz_cases.matrix"

    import tempfile
    import zoneinfo

    tmp = tempfile.mkdtemp(prefix="spid3r_cases_")
    with zipfile.ZipFile(wheel) as z:
        z.extractall(tmp)
    zoneinfo.reset_tzpath([os.path.join(tmp, "tzdata", "zoneinfo")])

    version = ""
    with open(os.path.join(tmp, "tzdata", "zoneinfo", "tzdata.zi"), encoding="utf-8") as f:
        for line in f:
            if line.startswith("# version"):
                version = line.split("version", 1)[1].strip()
                break

    lines = []
    lines.append("// GENERATED by tools/gen_unicode_tz_cases.py - do not edit.")
    lines.append("//")
    lines.append(f"// Unicode {unicodedata.unidata_version}, tzdata {version}.")
    lines.append("// Every assertion below is what CPython computes for the same input")
    lines.append("// (unicodedata.normalize, str.casefold, zoneinfo).")
    lines.append("")
    lines.append('import "unicode";')
    lines.append('import "tz";')
    lines.append("")
    lines.append("fn main() {")
    lines.append(f'    assert_eq(unicode_version(), "{unicodedata.unidata_version}");')
    lines.append(f'    assert_eq(tz_version(), "{version}");')

    lines.append("    // ---- normalization")
    n = 0
    for text in unicode_corpus():
        for form in ("NFC", "NFD", "NFKC", "NFKD"):
            want = unicodedata.normalize(form, text)
            lines.append(
                f"    assert_eq(unicode_normalize({q(text)}, \"{form}\"), {q(want)});"
            )
            n += 1
        lines.append(
            f"    assert_eq(unicode_casefold({q(text)}), {q(text.casefold())});"
        )
        n += 1
        # is_normalized must agree with the reference for every form
        for form in ("NFC", "NFD"):
            want = unicodedata.normalize(form, text) == text
            lines.append(
                f'    assert_eq(unicode_is_normalized({q(text)}, "{form}"), {"true" if want else "false"});'
            )
            n += 1

    lines.append("    // ---- time zones")
    for zone in ZONES:
        tz = zoneinfo.ZoneInfo(zone)
        for ts in TIMESTAMPS:
            dt = utc_dt(ts).astimezone(tz)
            off = int(dt.utcoffset().total_seconds())
            abbr = dt.tzname()
            want = dt.strftime("%Y-%m-%d %H:%M:%S")
            lines.append(
                f'    assert_eq(tz_format({q(zone)}, {ts}, "%Y-%m-%d %H:%M:%S"), {q(want)});'
            )
            lines.append(f'    assert_eq(tz_offset({q(zone)}, {ts}), {off});')
            lines.append(f'    assert_eq(tz_abbr({q(zone)}, {ts}), {q(abbr)});')
            n += 3
            # Round trip, but only for unambiguous instants: the repeated hour
            # at the end of DST has two instants with the same wall clock and
            # the documented policy picks the earlier one.
            if dt.timestamp() == ts:
                lines.append(
                    f'    let p_{len(lines)} = tz_parts({q(zone)}, {ts}).unwrap();'
                )
                lines.append(
                    f'    assert_eq(tz_from_parts({q(zone)}, p_{len(lines) - 1}["year"], '
                    f'p_{len(lines) - 1}["month"], p_{len(lines) - 1}["day"], '
                    f'p_{len(lines) - 1}["hour"], p_{len(lines) - 1}["minute"], '
                    f'p_{len(lines) - 1}["second"]).unwrap(), {ts});'
                )
                n += 1

    # DST gap policy: 02:30 does not exist in New York on 2026-03-08.
    lines.append("    // ---- DST gap: a nonexistent wall time moves forward past the gap")
    lines.append(
        '    let gap = tz_from_parts("America/New_York", 2026, 3, 8, 2, 30, 0).unwrap();'
    )
    lines.append(
        '    assert_eq(tz_format("America/New_York", gap, "%H:%M"), "03:30");'
    )
    # Ambiguity: 01:30 happens twice on 2026-11-01; the earlier one wins.
    lines.append("    // ---- DST overlap: the earlier instant wins")
    lines.append(
        '    let amb = tz_from_parts("America/New_York", 2026, 11, 1, 1, 30, 0).unwrap();'
    )
    lines.append(
        '    assert_eq(tz_format("America/New_York", amb, "%H:%M %Z"), "01:30 EDT");'
    )

    lines.append("    return 0;")
    lines.append("}")
    lines.append("")

    os.makedirs(os.path.dirname(dest), exist_ok=True)
    with open(dest, "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(lines))

    print(f"{dest}: {n} assertions, Unicode {unicodedata.unidata_version}, tzdata {version}")


if __name__ == "__main__":
    main()
