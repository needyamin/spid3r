//! IANA time zone support, backed by a generated transition table.
//!
//! `tzdata.bin` is produced by `tools/gen_tzdata.py` from the compiled tz
//! database (RFC 8536 TZif). Two properties matter:
//!
//!   * It is **expanded**, not copied. Modern TZif builds are "slim": they
//!     stop emitting transitions once a POSIX TZ footer describes them
//!     (Europe/Berlin's last explicit transition is 1996). The generator
//!     evaluates that footer and synthesises every transition through 2100, so
//!     this reader never has to implement POSIX rules - it only binary-searches
//!     a sorted array, which is both faster and much harder to get wrong.
//!
//!   * It is **verified**. Every zone is compared against CPython's `zoneinfo`
//!     (the same TZif files) at every transition ±1 s plus a monthly sweep from
//!     1900 to 2100; a mismatch aborts generation.
//!
//! Coverage: transitions are authoritative for 1900-01-01 .. 2100-12-31.
//! Outside that window the nearest transition's type is held, which is exact
//! for fixed-offset zones and an approximation beyond 2100 for DST zones.
//!
//! The blob is parsed once into a `HashMap` on first use (`OnceLock`), so a
//! script that never touches time zones pays nothing beyond the bytes sitting
//! in the binary's rodata.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::datetime;
use super::NativeDef;
use crate::value::collections;
use crate::value::{type_name, NativeCtx, Value};

/// The generated database. See `tools/gen_tzdata.py` for the format.
static BLOB: &[u8] = include_bytes!("tzdata.bin");

const MAGIC: &[u8; 5] = b"SPTZ1";

pub fn native_defs() -> Vec<NativeDef> {
    vec![
        ("tz_names", n_tz_names),
        ("tz_info", n_tz_info),
        ("tz_parts", n_tz_parts),
        ("tz_from_parts", n_tz_from_parts),
        ("tz_format", n_tz_format),
        ("tz_version", n_tz_version),
    ]
}

// ------------------------------------------------------------ model

struct LocalType {
    offset: i32,
    isdst: bool,
    abbr: String,
}

struct Zone {
    types: Vec<LocalType>,
    /// Type in effect before the first transition.
    initial: usize,
    /// Sorted transition instants (UTC seconds).
    times: Vec<i64>,
    /// Local-type index for each transition.
    kind: Vec<u8>,
}

impl Zone {
    fn local_type(&self, t: i64) -> &LocalType {
        let idx = match self.times.binary_search(&t) {
            Ok(i) => i,
            // Before the first transition: the zone's initial type.
            Err(0) => return &self.types[self.initial],
            Err(i) => i - 1,
        };
        &self.types[self.kind[idx] as usize]
    }

    fn offset_at(&self, t: i64) -> i32 {
        self.local_type(t).offset
    }
}

struct Db {
    version: String,
    zones: Vec<Zone>,
    /// Every name - canonical zones and links - to a zone index.
    index: HashMap<String, usize>,
}

// ------------------------------------------------------------ blob reader

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf, pos: 0 }
    }

    fn u8(&mut self) -> u8 {
        let v = *self.buf.get(self.pos).unwrap_or(&0);
        self.pos += 1;
        v
    }

    fn u16(&mut self) -> u16 {
        let lo = self.u8() as u16;
        let hi = self.u8() as u16;
        lo | (hi << 8)
    }

    fn i32(&mut self) -> i32 {
        let b0 = self.u8() as u32;
        let b1 = self.u8() as u32;
        let b2 = self.u8() as u32;
        let b3 = self.u8() as u32;
        (b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)) as i32
    }

    fn bytes(&mut self, n: usize) -> &'a [u8] {
        let end = (self.pos + n).min(self.buf.len());
        let slice = &self.buf[self.pos.min(self.buf.len())..end];
        self.pos = end;
        slice
    }

    fn text_u16(&mut self) -> String {
        let n = self.u16() as usize;
        String::from_utf8_lossy(self.bytes(n)).into_owned()
    }

    fn text_u8(&mut self) -> String {
        let n = self.u8() as usize;
        String::from_utf8_lossy(self.bytes(n)).into_owned()
    }

    fn varint(&mut self) -> u64 {
        let mut out: u64 = 0;
        let mut shift = 0;
        loop {
            let b = self.u8();
            out |= ((b & 0x7F) as u64) << shift;
            if b & 0x80 == 0 || shift >= 63 {
                return out;
            }
            shift += 7;
        }
    }
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

// ------------------------------------------------------------ parse

fn parse() -> Db {
    let mut r = Reader::new(BLOB);
    let magic = r.bytes(5);
    debug_assert_eq!(magic, MAGIC, "tzdata.bin has a bad magic number");

    let version = r.text_u16();
    let zone_count = r.u16() as usize;
    let link_count = r.u16() as usize;

    let mut zones = Vec::with_capacity(zone_count);
    let mut index: HashMap<String, usize> = HashMap::with_capacity(zone_count + link_count);
    let mut names = Vec::with_capacity(zone_count);

    for _ in 0..zone_count {
        let name = r.text_u16();
        let type_count = r.u8() as usize;
        let mut types = Vec::with_capacity(type_count);
        for _ in 0..type_count {
            let offset = r.i32();
            let isdst = r.u8() != 0;
            let abbr = r.text_u8();
            types.push(LocalType {
                offset,
                isdst,
                abbr,
            });
        }
        let initial = (r.u8() as usize).min(type_count.saturating_sub(1));
        let trans_count = r.u16() as usize;
        let mut times = Vec::with_capacity(trans_count);
        let mut kind = Vec::with_capacity(trans_count);
        let mut prev: i64 = 0;
        for _ in 0..trans_count {
            prev = prev.saturating_add(unzigzag(r.varint()));
            times.push(prev);
            kind.push(r.u8());
        }
        names.push(name);
        zones.push(Zone {
            types,
            initial,
            times,
            kind,
        });
    }

    for (i, name) in names.iter().enumerate() {
        index.insert(name.clone(), i);
    }
    for _ in 0..link_count {
        let name = r.text_u16();
        let target = r.u16() as usize;
        if target < zones.len() {
            index.insert(name, target);
        }
    }

    Db {
        version,
        zones,
        index,
    }
}

fn db() -> &'static Db {
    static DB: OnceLock<Db> = OnceLock::new();
    DB.get_or_init(parse)
}

fn find(name: &str) -> Option<&'static Zone> {
    let d = db();
    d.index.get(name).map(|&i| &d.zones[i])
}

// ------------------------------------------------------------ conversions

/// `(local_epoch, offset, abbr, isdst)` for a UTC instant in a zone.
fn localize(zone: &Zone, t: i64) -> (i64, i32, &str, bool) {
    let lt = zone.local_type(t);
    (
        t.saturating_add(lt.offset as i64),
        lt.offset,
        lt.abbr.as_str(),
        lt.isdst,
    )
}

/// Civil parts -> UTC instant, resolving DST.
///
/// A local time can be ambiguous (the hour that repeats when DST ends) or
/// nonexistent (the hour skipped when DST starts). Ambiguity resolves to the
/// earlier instant - the same choice CPython's `fold=0` makes. A nonexistent
/// time is pushed forward by the size of the gap, which is what every
/// civil-time library does rather than returning an error for a time a user
/// typed by hand.
fn from_local(zone: &Zone, local: i64) -> i64 {
    let before = zone.offset_at(local - 86_400);
    let after = zone.offset_at(local + 86_400);

    let mut best: Option<i64> = None;
    for off in [before, after] {
        let t = local - off as i64;
        if zone.offset_at(t) == off {
            best = Some(match best {
                Some(b) => b.min(t),
                None => t,
            });
        }
    }
    match best {
        Some(t) => t,
        // Gap: interpret the wall time with the pre-transition offset, which
        // lands just after the transition.
        None => local - before as i64,
    }
}

// ------------------------------------------------------------ value helpers

fn arity(who: &str, args: &[Value], n: usize) -> Result<(), String> {
    if args.len() == n {
        Ok(())
    } else {
        Err(format!(
            "{} expects {} argument{}, got {}",
            who,
            n,
            if n == 1 { "" } else { "s" },
            args.len()
        ))
    }
}

fn want_str(v: &Value, who: &str) -> Result<String, String> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(format!(
            "{} expects a string, found {}",
            who,
            type_name(other)
        )),
    }
}

fn want_int(v: &Value, who: &str) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(*i),
        other => Err(format!(
            "{} expects an int, found {}",
            who,
            type_name(other)
        )),
    }
}

fn str_val(s: impl Into<String>) -> Value {
    Value::Str(s.into().into())
}

fn map_of(pairs: Vec<(&str, Value)>) -> Value {
    let mut flat = Vec::with_capacity(pairs.len() * 2);
    for (k, v) in pairs {
        flat.push(str_val(k));
        flat.push(v);
    }
    collections::construct("map", flat).expect("map construction cannot fail")
}

fn ok(v: Value) -> Result<Value, String> {
    collections::construct("ok", vec![v])
}

fn err(msg: impl Into<String>) -> Result<Value, String> {
    collections::construct("err", vec![str_val(msg.into())])
}

fn zone_arg(args: &[Value], who: &str) -> Result<(&'static Zone, String), String> {
    let name = want_str(&args[0], who)?;
    match find(&name) {
        Some(z) => Ok((z, name)),
        None => Err(format!(
            "{}: unknown time zone '{}' (try tz_names())",
            who, name
        )),
    }
}

// ------------------------------------------------------------ natives

/// `tz_names()` -> sorted list of every zone and link name.
fn n_tz_names(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("tz_names", &args, 0)?;
    let d = db();
    let mut names: Vec<&str> = d.index.keys().map(|s| s.as_str()).collect();
    names.sort_unstable();
    let items: Vec<Value> = names.into_iter().map(str_val).collect();
    collections::construct("list", items)
}

/// `tz_info(zone, unix_secs)` -> ok({zone, offset, abbr, isdst}).
fn n_tz_info(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("tz_info", &args, 2)?;
    let (zone, name) = match zone_arg(&args, "tz_info") {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let t = want_int(&args[1], "tz_info")?;
    let lt = zone.local_type(t);
    ok(map_of(vec![
        ("zone", str_val(name)),
        ("offset", Value::Int(lt.offset as i64)),
        ("abbr", str_val(lt.abbr.clone())),
        ("isdst", Value::Bool(lt.isdst)),
    ]))
}

/// `tz_parts(zone, unix_secs)` -> ok(civil parts + offset/abbr/isdst).
fn n_tz_parts(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("tz_parts", &args, 2)?;
    let (zone, name) = match zone_arg(&args, "tz_parts") {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let t = want_int(&args[1], "tz_parts")?;
    let (local, offset, abbr, isdst) = localize(zone, t);
    let (y, mo, d, h, mi, s, w) = datetime::datetime_parts(local);
    ok(map_of(vec![
        ("zone", str_val(name)),
        ("year", Value::Int(y)),
        ("month", Value::Int(mo as i64)),
        ("day", Value::Int(d as i64)),
        ("hour", Value::Int(h)),
        ("minute", Value::Int(mi)),
        ("second", Value::Int(s)),
        ("weekday", Value::Int(w)),
        ("offset", Value::Int(offset as i64)),
        ("abbr", str_val(abbr.to_string())),
        ("isdst", Value::Bool(isdst)),
    ]))
}

/// `tz_from_parts(zone, y, mo, d, h, mi, s)` -> ok(unix_secs).
fn n_tz_from_parts(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("tz_from_parts", &args, 7)?;
    let (zone, _) = match zone_arg(&args, "tz_from_parts") {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let mut parts = [0i64; 6];
    for (i, slot) in parts.iter_mut().enumerate() {
        *slot = want_int(&args[i + 1], "tz_from_parts")?;
    }
    let local = match datetime::datetime_from_parts(
        parts[0], parts[1], parts[2], parts[3], parts[4], parts[5],
    ) {
        Ok(v) => v,
        Err(e) => return err(format!("tz_from_parts: {}", e)),
    };
    ok(Value::Int(from_local(zone, local)))
}

/// `tz_format(zone, unix_secs, fmt)` -> formatted local time.
fn n_tz_format(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("tz_format", &args, 3)?;
    let (zone, _) = match zone_arg(&args, "tz_format") {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let t = want_int(&args[1], "tz_format")?;
    let fmt = want_str(&args[2], "tz_format")?;
    let (local, offset, abbr, _) = localize(zone, t);
    Ok(str_val(datetime::format_parts(local, offset, abbr, &fmt)))
}

/// `tz_version()` -> the tzdata release the table was generated from.
fn n_tz_version(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("tz_version", &args, 0)?;
    Ok(str_val(db().version.clone()))
}
