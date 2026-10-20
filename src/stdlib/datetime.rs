//! Zero-dependency DateTime builtins using Howard Hinnant's civil calendar algorithm.
//!
//! Provides epoch second and millisecond timestamps, civil calendar parts breakdown,
//! civil parts to epoch timestamp conversion, and strftime-style formatting.

use std::time::{SystemTime, UNIX_EPOCH};

/// Days since 1970-01-01 -> (year, month 1..12, day 1..31)
/// Howard Hinnant's algorithm.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = (yoe as i64) + era * 400;
    let doy = (doe - (365 * yoe + yoe / 4 - yoe / 100)) as u32; // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = y + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

/// (year, month 1..12, day 1..31) -> days since 1970-01-01
/// Howard Hinnant's algorithm.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as u64; // [0, 146096]
    era * 146097 + (doe as i64) - 719468
}

/// Current system time in Unix epoch seconds.
pub fn unix_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Current system time in Unix epoch milliseconds.
pub fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Decompose epoch seconds into (year, month 1..12, day 1..31, hour 0..23, min 0..59, sec 0..59, weekday 0..6 where 0=Sunday).
pub fn datetime_parts(epoch_sec: i64) -> (i64, u32, u32, i64, i64, i64, i64) {
    let days = epoch_sec.div_euclid(86400);
    let rem = epoch_sec.rem_euclid(86400);
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;
    let (year, month, day) = civil_from_days(days);
    // 1970-01-01 was Thursday (4). 4 % 7 = 4.
    let weekday = (days + 4).rem_euclid(7);
    (year, month, day, hour, minute, second, weekday)
}

/// Convert civil parts (year, month 1..12, day 1..31, hour, minute, second) to epoch seconds.
pub fn datetime_from_parts(
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
) -> Result<i64, String> {
    if !(-100_000_000..=100_000_000).contains(&year) {
        return Err(format!("year out of range, got {}", year));
    }
    if !(1..=12).contains(&month) {
        return Err(format!("month must be between 1 and 12, got {}", month));
    }
    if !(1..=31).contains(&day) {
        return Err(format!("day must be between 1 and 31, got {}", day));
    }
    if !(0..=23).contains(&hour) {
        return Err(format!("hour must be between 0 and 23, got {}", hour));
    }
    if !(0..=59).contains(&minute) {
        return Err(format!("minute must be between 0 and 59, got {}", minute));
    }
    if !(0..=60).contains(&second) {
        return Err(format!("second must be between 0 and 60, got {}", second));
    }
    let days = days_from_civil(year, month as u32, day as u32);
    let sec = days
        .checked_mul(86400)
        .and_then(|s| s.checked_add(hour * 3600 + minute * 60 + second))
        .ok_or_else(|| "datetime_from_parts: timestamp overflow".to_string())?;
    Ok(sec)
}

/// Format an epoch timestamp with strftime-like specifiers:
/// %Y (4-digit year), %m (2-digit month), %d (2-digit day),
/// %H (2-digit hour 24h), %M (2-digit minute), %S (2-digit second),
/// %w (weekday 0=Sun), %F (%Y-%m-%d), %T (%H:%M:%S), %% (%)
pub fn format_datetime(epoch_sec: i64, fmt_str: &str) -> String {
    let (year, month, day, hour, minute, second, weekday) = datetime_parts(epoch_sec);
    let mut out = String::new();
    let chars: Vec<char> = fmt_str.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' && i + 1 < chars.len() {
            i += 1;
            match chars[i] {
                'Y' => out.push_str(&format!("{:04}", year)),
                'm' => out.push_str(&format!("{:02}", month)),
                'd' => out.push_str(&format!("{:02}", day)),
                'H' => out.push_str(&format!("{:02}", hour)),
                'M' => out.push_str(&format!("{:02}", minute)),
                'S' => out.push_str(&format!("{:02}", second)),
                'w' => out.push_str(&weekday.to_string()),
                'F' => out.push_str(&format!("{:04}-{:02}-{:02}", year, month, day)),
                'T' => out.push_str(&format!("{:02}:{:02}:{:02}", hour, minute, second)),
                '%' => out.push('%'),
                other => {
                    out.push('%');
                    out.push(other);
                }
            }
        } else {
            out.push(chars[i]);
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_epoch_zero() {
        let (y, m, d, h, min, s, w) = datetime_parts(0);
        assert_eq!(y, 1970);
        assert_eq!(m, 1);
        assert_eq!(d, 1);
        assert_eq!(h, 0);
        assert_eq!(min, 0);
        assert_eq!(s, 0);
        assert_eq!(w, 4); // Thursday
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn test_round_trip() {
        let ts = 1761000000; // 2025-10-20
        let (y, m, d, h, min, s, _) = datetime_parts(ts);
        let back = datetime_from_parts(y, m as i64, d as i64, h, min, s).unwrap();
        assert_eq!(ts, back);
    }

    #[test]
    fn test_formatting() {
        let ts = 0; // 1970-01-01 00:00:00
        let s = format_datetime(ts, "%Y-%m-%d %H:%M:%S");
        assert_eq!(s, "1970-01-01 00:00:00");
    }
}
