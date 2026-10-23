#!/usr/bin/env python3
"""Generate src/stdlib/tzdata.bin from an IANA tzdata build (TZif files).

    python tools/gen_tzdata.py [path-to-tzdata.whl | path-to-zoneinfo-dir]

The input is the compiled form of the tz database (RFC 8536 TZif), which is
what `zic` produces. The PyPI `tzdata` wheel is just a zip of those files, so
`pip download tzdata` is enough; the tz database itself is public domain.

Two things make this more than a copy:

1. **Slim TZif.** Modern builds (`zic -b slim`) omit every transition that the
   POSIX TZ footer already describes - Europe/Berlin's last explicit
   transition is 1996, and 1996-2100 comes from `CET-1CEST,M3.5.0,M10.5.0/3`.
   Copying the file would therefore be wrong for the last 30 years, not just
   the future. This script parses the footer and *synthesises* the transitions
   through 2100, so the runtime only ever does a binary search.

2. **Deduplication.** The wheel materialises links as full copies
   (Asia/Calcutta and Asia/Kolkata are byte-identical). Identical timelines
   collapse to one zone record plus a link entry.

Every zone is then verified against CPython's `zoneinfo` (which reads the same
TZif files) at thousands of instants; see --verify. A mismatch aborts the
generation instead of shipping a wrong offset.

Output format (little-endian, see src/stdlib/tz.rs for the reader):

    "SPTZ1" | u16 version_len, version | u16 zone_count | u16 link_count
    zone  := u16 name_len, name
             | u8 type_count, type_count * (i32 offset, u8 isdst,
                                            u8 abbr_len, abbr)
             | u8 initial_type_index
             | u16 trans_count,
               trans_count * (varint zigzag delta_secs, u8 type_index)
    link  := u16 name_len, name | u16 zone_index
"""

import io
import os
import struct
import sys
import zipfile
from datetime import datetime, timedelta, timezone

EPOCH = datetime(1970, 1, 1, tzinfo=timezone.utc)
RANGE_START = -2208988800  # 1900-01-01T00:00:00Z
RANGE_END = 4133980800  # 2100-12-31T00:00:00Z
MAX_TRANSITIONS = 65535

def utc_dt(t):
    """Epoch seconds -> aware UTC datetime.

    Not `datetime.fromtimestamp`: on Windows that fails for negative values,
    i.e. for the whole 1900-1969 range this generator cares about.
    """
    return EPOCH + timedelta(seconds=t)


# ---------------------------------------------------------------- TZif


def parse_tzif(data):
    """-> (times, type_indices, types, footer) or None when not a TZif file.

    types: list of (utoff_secs, isdst, abbr)
    """
    if data[:4] != b"TZif":
        return None
    version = data[4:5]

    def header(off):
        counts = struct.unpack(">6I", data[off + 20 : off + 44])
        return counts + (off + 44,)

    isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt, p = header(0)
    p += timecnt * 4 + timecnt + typecnt * 6 + charcnt + leapcnt * 8 + isstdcnt + isutcnt

    if version not in (b"2", b"3", b"4"):
        # A v1 file has no 64-bit block and no footer. Treat it as UTC-only;
        # the wheel never ships one.
        return [], [], [(0, 0, "UTC")], b""

    isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt, p = header(p)
    times = list(struct.unpack(">%dq" % timecnt, data[p : p + 8 * timecnt]))
    p += 8 * timecnt
    indices = list(data[p : p + timecnt])
    p += timecnt

    raw_types = []
    for _ in range(typecnt):
        utoff, isdst, abbrind = struct.unpack(">iBB", data[p : p + 6])
        p += 6
        raw_types.append((utoff, isdst, abbrind))

    abbrs = data[p : p + charcnt]
    p += charcnt + leapcnt * 8 + isstdcnt + isutcnt

    types = []
    for utoff, isdst, abbrind in raw_types:
        end = abbrs.find(b"\0", abbrind)
        if end < 0:
            end = len(abbrs)
        abbr = abbrs[abbrind:end].decode("ascii", "replace")
        types.append((utoff, isdst, abbr))

    footer = data[p:].strip(b"\n").decode("ascii", "replace")
    return times, indices, types, footer


# ---------------------------------------------------------------- POSIX TZ


def parse_hms(text):
    """`[+-]hh[:mm[:ss]]` -> seconds (sign preserved)."""
    sign = 1
    if text.startswith("-"):
        sign = -1
        text = text[1:]
    elif text.startswith("+"):
        text = text[1:]
    parts = text.split(":")
    hh = int(parts[0]) if parts[0] else 0
    mm = int(parts[1]) if len(parts) > 1 else 0
    ss = int(parts[2]) if len(parts) > 2 else 0
    return sign * (hh * 3600 + mm * 60 + ss)


def parse_tz_name(s, i):
    if s[i] == "<":
        end = s.index(">", i)
        return s[i + 1 : end], end + 1
    j = i
    while j < len(s) and (s[j].isalpha()):
        j += 1
    if j == i:
        raise ValueError("empty timezone name in " + s)
    return s[i:j], j


def parse_rule(text):
    """`Jn` | `n` | `Mm.w.d`, plus optional `/time` -> (kind, params, time_secs)."""
    if "/" in text:
        body, when = text.split("/", 1)
        at = parse_hms(when)
    else:
        body, at = text, 7200  # POSIX default: 02:00 local
    if body.startswith("J"):
        return ("J", (int(body[1:]),), at)
    if body.startswith("M"):
        m, w, d = body[1:].split(".")
        return ("M", (int(m), int(w), int(d)), at)
    return ("N", (int(body),), at)


def scan_offset(s, i):
    """Scan `[+-]hh[:mm[:ss]]` and return (text, next_index)."""
    start = i
    if i < len(s) and s[i] in "+-":
        i += 1
    while i < len(s) and (s[i].isdigit() or s[i] == ":"):
        i += 1
    return s[start:i], i


def parse_posix_tz(spec):
    """-> dict(std, std_off, dst, dst_off, start, end) or a fixed-zone dict.

    POSIX offsets are positive *west* of Greenwich, so the UTC offset is the
    negation of the parsed value: `CET-1` means UTC+01:00.
    """
    i = 0
    std, i = parse_tz_name(spec, i)
    text, i = scan_offset(spec, i)
    if text in ("", "+", "-"):
        raise ValueError("missing standard offset in " + spec)
    std_off = -parse_hms(text)
    if i >= len(spec):
        return {"std": std, "std_off": std_off, "dst": None}

    dst, i = parse_tz_name(spec, i)
    dst_off = std_off + 3600
    if i < len(spec) and spec[i] != ",":
        text, i = scan_offset(spec, i)
        if text in ("", "+", "-"):
            raise ValueError("missing DST offset in " + spec)
        dst_off = -parse_hms(text)
    if i >= len(spec) or spec[i] != ",":
        raise ValueError("DST name without rules: " + spec)

    start_s, end_s = spec[i + 1 :].split(",")
    return {
        "std": std,
        "std_off": std_off,
        "dst": dst,
        "dst_off": dst_off,
        "start": parse_rule(start_s),
        "end": parse_rule(end_s),
    }


def days_in_month(y, m):
    if m == 2:
        leap = (y % 4 == 0 and y % 100 != 0) or y % 400 == 0
        return 29 if leap else 28
    return 30 if m in (4, 6, 9, 11) else 31


def rule_local_date(year, rule):
    """The civil date the rule names in `year` (as days since 1970-01-01)."""
    kind, params, _ = rule
    if kind == "M":
        month, week, wday = params
        first = datetime(year, month, 1, tzinfo=timezone.utc)
        # weekday() is Monday=0; POSIX wants Sunday=0.
        first_wday = (first.weekday() + 1) % 7
        day = 1 + (wday - first_wday) % 7 + (week - 1) * 7
        while day > days_in_month(year, month):
            day -= 7
        return datetime(year, month, day, tzinfo=timezone.utc)
    if kind == "J":
        n = params[0]
        base = datetime(year, 1, 1, tzinfo=timezone.utc) + timedelta(days=n - 1)
        if ((year % 4 == 0 and year % 100 != 0) or year % 400 == 0) and n >= 60:
            base += timedelta(days=1)
        return base
    n = params[0]
    return datetime(year, 1, 1, tzinfo=timezone.utc) + timedelta(days=n)


def rule_utc(year, rule, offset_before):
    """The UTC instant of a rule firing, given the offset in effect before it."""
    _, _, at = rule
    local = rule_local_date(year, rule) + timedelta(seconds=at)
    return int(local.timestamp()) - offset_before


# ---------------------------------------------------------------- timeline


def timeline(times, indices, types, footer):
    """-> (initial_offset, [(utc, offset, isdst, abbr), ...]) for 1900..2100."""
    if not types:
        return (0, 0, "UTC"), []

    # Offset in effect before the first transition (RFC 8536 section 3.2):
    # the first standard-time type, else type 0.
    initial = next((t for t in types if not t[1]), types[0])

    entries = []
    for t, idx in zip(times, indices):
        if t < RANGE_START:
            initial = types[idx]
            continue
        if t > RANGE_END:
            break
        offset, isdst, abbr = types[idx]
        entries.append((t, offset, isdst, abbr))

    # Extend with the POSIX footer when the file is slim.
    if footer:
        rule = parse_posix_tz(footer)
        last_t = entries[-1][0] if entries else RANGE_START
        if rule["dst"] is None:
            if not entries:
                initial = (rule["std_off"], 0, rule["std"])
            # A fixed zone: nothing to generate.
        else:
            start_year = utc_dt(last_t).year - 1
            generated = []
            for year in range(start_year, 2101):
                s = rule_utc(year, rule["start"], rule["std_off"])
                e = rule_utc(year, rule["end"], rule["dst_off"])
                generated.append((s, rule["dst_off"], 1, rule["dst"]))
                generated.append((e, rule["std_off"], 0, rule["std"]))
            generated.sort()
            for t, off, isdst, abbr in generated:
                if t <= last_t or t > RANGE_END:
                    continue
                entries.append((t, off, isdst, abbr))

    entries.sort(key=lambda e: e[0])
    # Drop same-instant duplicates (a footer rule can coincide with an
    # explicit transition at the boundary).
    deduped = []
    for e in entries:
        if deduped and deduped[-1][0] == e[0]:
            deduped[-1] = e
        else:
            deduped.append(e)
    if len(deduped) > MAX_TRANSITIONS:
        raise SystemExit(f"too many transitions: {len(deduped)}")
    return initial, deduped


# ---------------------------------------------------------------- verify


def verify(name, initial, entries):
    """Compare every sample against CPython's zoneinfo (same TZif files)."""
    from bisect import bisect_right
    from zoneinfo import ZoneInfo

    tz = ZoneInfo(name)
    times = [e[0] for e in entries]
    offsets = [e[1] for e in entries]

    samples = [RANGE_START, 0, 946684800, 1761000000]
    for t in times:
        samples.extend((t - 1, t, t + 1))
    samples.extend(range(RANGE_START, RANGE_END, 30 * 86400))

    for t in samples:
        i = bisect_right(times, t) - 1
        got = offsets[i] if i >= 0 else initial[0]
        # astimezone, not utcoffset(dt): utcoffset interprets its argument as
        # *local* wall time in the zone, which silently shifts every sample.
        want = int(utc_dt(t).astimezone(tz).utcoffset().total_seconds())
        if got != want:
            raise SystemExit(
                f"{name}: offset mismatch at {t} "
                f"({utc_dt(t)}): "
                f"generated {got}, zoneinfo {want}"
            )


# ---------------------------------------------------------------- emit


def varint(n):
    """Unsigned LEB128."""
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def zigzag(v):
    return (v << 1) ^ (v >> 63) if v < 0 else (v << 1)


def tzdata_version(path):
    """The `# version 2026e` line from tzdata.zi, when the input has one."""
    try:
        if os.path.isdir(path):
            for cand in ("tzdata.zi", os.path.join("zoneinfo", "tzdata.zi")):
                full = os.path.join(path, cand)
                if os.path.exists(full):
                    with open(full, "r", encoding="utf-8") as f:
                        return first_version_line(f.read())
            return ""
        with zipfile.ZipFile(path) as z:
            for n in z.namelist():
                if n.endswith("tzdata.zi"):
                    return first_version_line(z.read(n).decode("utf-8", "replace"))
    except (OSError, KeyError):
        pass
    return ""


def first_version_line(text):
    for line in text.splitlines():
        if line.startswith("# version"):
            return line.split("version", 1)[1].strip()
    return ""


def load_zones(path):
    """-> {name: TZif bytes}"""
    zones = {}
    if os.path.isdir(path):
        root = path
        if os.path.isdir(os.path.join(path, "zoneinfo")):
            root = os.path.join(path, "zoneinfo")
        for dirpath, _, files in os.walk(root):
            for fn in files:
                full = os.path.join(dirpath, fn)
                rel = os.path.relpath(full, root).replace(os.sep, "/")
                with open(full, "rb") as f:
                    data = f.read()
                if data[:4] == b"TZif":
                    zones[rel] = data
        return zones

    with zipfile.ZipFile(path) as z:
        for n in z.namelist():
            if "/zoneinfo/" not in n or n.endswith("/"):
                continue
            data = z.read(n)
            if data[:4] != b"TZif":
                continue
            zones[n.split("/zoneinfo/", 1)[1]] = data
    return zones


def main():
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    src = sys.argv[1]
    dest = sys.argv[2] if len(sys.argv) > 2 else "src/stdlib/tzdata.bin"
    do_verify = "--no-verify" not in sys.argv

    zones = load_zones(src)
    if not zones:
        raise SystemExit(f"no TZif files found in {src}")

    built = {}
    for name, data in sorted(zones.items()):
        parsed = parse_tzif(data)
        if parsed is None:
            continue
        initial, entries = timeline(*parsed)
        built[name] = (initial, entries)

    # Deduplicate identical timelines into links.
    canonical = {}
    order = []
    links = []
    for name, (initial, entries) in built.items():
        key = (initial, tuple(entries))
        if key in canonical:
            links.append((name, canonical[key]))
        else:
            canonical[key] = name
            order.append(name)

    if do_verify:
        if os.path.isdir(src):
            import zoneinfo

            root = src if os.path.isdir(os.path.join(src, "zoneinfo")) else os.path.join(src, "zoneinfo")
            zoneinfo.reset_tzpath([root])
        else:
            import tempfile

            tmp = tempfile.mkdtemp(prefix="spid3r_tz_")
            with zipfile.ZipFile(src) as z:
                z.extractall(tmp)
            import zoneinfo

            zoneinfo.reset_tzpath([os.path.join(tmp, "tzdata", "zoneinfo")])
        for i, name in enumerate(order):
            verify(name, *built[name])
            if i % 50 == 0:
                print(f"  verified {i}/{len(order)}", file=sys.stderr)

    version = tzdata_version(src).encode("ascii", "replace")
    out = io.BytesIO()
    out.write(b"SPTZ1")
    out.write(struct.pack("<H", len(version)))
    out.write(version)
    out.write(struct.pack("<HH", len(order), len(links)))

    for name in order:
        initial, entries = built[name]
        nb = name.encode("utf-8")
        out.write(struct.pack("<H", len(nb)))
        out.write(nb)
        # Intern the local-time types this zone actually uses, starting with
        # the one in effect before the first transition.
        used = []
        for off, isdst, abbr in [initial] + [(e[1], e[2], e[3]) for e in entries]:
            key = (off, isdst, abbr)
            if key not in used:
                used.append(key)
        index_of = {k: i for i, k in enumerate(used)}
        if len(used) > 255:
            raise SystemExit(f"{name}: more than 255 local-time types")
        out.write(struct.pack("<B", len(used)))
        for off, isdst, abbr in used:
            ab = abbr.encode("ascii", "replace")
            out.write(struct.pack("<i", off))
            out.write(struct.pack("<B", isdst))
            out.write(struct.pack("<B", len(ab)))
            out.write(ab)
        out.write(struct.pack("<B", used.index(initial)))
        out.write(struct.pack("<H", len(entries)))
        prev = 0
        for t, off, isdst, abbr in entries:
            out.write(varint(zigzag(t - prev)))
            out.write(struct.pack("<B", index_of[(off, isdst, abbr)]))
            prev = t

    for name, target in links:
        nb = name.encode("utf-8")
        out.write(struct.pack("<H", len(nb)))
        out.write(nb)
        out.write(struct.pack("<H", order.index(target)))

    blob = out.getvalue()
    with open(dest, "wb") as f:
        f.write(blob)

    total = sum(len(built[n][1]) for n in order)
    print(
        f"{dest}: tzdata {version.decode() or 'unknown'}, "
        f"{len(order)} zones + {len(links)} links, "
        f"{total} transitions, {len(blob)} bytes"
    )


if __name__ == "__main__":
    main()
