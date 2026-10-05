//! Time values: seconds since 1970-01-01 00:00:00 UTC, without leap seconds.

/// Days from 1970-01-01 to a date of the Gregorian calendar.
pub fn days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let (era, yoe) = (y.div_euclid(400), y.rem_euclid(400));
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}

/// Time of a text: "2026-09-22T10:27:01Z", "2026-09-22 10:27", "2026-09-22" or "20260922T102701".
// ponytail: UTC only. A UTC offset in the text ("+02:00") is ignored.
pub fn parse(s: &str) -> Option<f64> {
    let (date, time) = s.trim().split_once(['T', ' ']).unwrap_or((s.trim(), ""));
    let num = |t: &str| t.trim().parse::<f64>().ok();
    let (y, m, d) = if date.contains('-') {
        let mut p = date.splitn(3, '-');
        (num(p.next()?)?, num(p.next()?)?, num(p.next()?)?)
    } else if date.len() == 8 && date.is_ascii() {
        (num(&date[..4])?, num(&date[4..6])?, num(&date[6..])?)
    } else {
        return None;
    };
    let t = time.trim_end_matches('Z').split('+').next().unwrap_or("");
    let (h, mi, sec) = if t.contains(':') {
        let mut p = t.split(':');
        (num(p.next()?)?, p.next().and_then(num).unwrap_or(0.0), p.next().and_then(num).unwrap_or(0.0))
    } else if t.len() >= 6 && t.is_ascii() {
        (num(&t[..2])?, num(&t[2..4])?, num(&t[4..])?)
    } else {
        (0.0, 0.0, 0.0)
    };
    Some(days(y as i64, m as i64, d as i64) as f64 * 86400.0 + h * 3600.0 + mi * 60.0 + sec)
}

/// CF time units, for example "days since 1970-01-01 00:00:00": (seconds for one unit, time of the value 0).
pub fn cf(units: &str) -> Option<(f64, f64)> {
    let (unit, origin) = units.split_once(" since ")?;
    let k = match unit.trim().to_lowercase().trim_end_matches('s') {
        "nanosecond" | "n" => 1e-9,
        "microsecond" | "u" => 1e-6,
        "millisecond" | "m" => 1e-3,
        "second" | "sec" | "" => 1.0,
        "minute" | "min" => 60.0,
        "hour" | "hr" | "h" => 3600.0,
        "day" | "d" => 86400.0,
        _ => return None,
    };
    Some((k, parse(origin)?))
}

/// Text of a time: "2026-09-22 10:27:01".
pub fn text(t: f64) -> String {
    let (d, s) = ((t / 86400.0).floor() as i64, t.rem_euclid(86400.0) as i64);
    let z = d + 719468;
    let (era, doe) = (z.div_euclid(146097), z.rem_euclid(146097));
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (day, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{day:02} {:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Time in a file name: the first date "YYYYMMDD" (years 1900 to 2099), with the time if "THHMMSS" or
/// "HHMMSS" follows. For example "S2A_MSIL2A_20260922T102701_N0513".
pub fn in_name(name: &str) -> Option<f64> {
    let b = name.as_bytes();
    let digits = |i: usize, n: usize| b.get(i..i + n).filter(|s| s.iter().all(u8::is_ascii_digit)).map(|s| std::str::from_utf8(s).unwrap());
    for i in 0..b.len() {
        let Some(d) = digits(i, 8) else { continue };
        let ok = (d.starts_with("19") || d.starts_with("20")) && ("01"..="12").contains(&&d[4..6]) && ("01"..="31").contains(&&d[6..8]);
        if !ok || (i > 0 && b[i - 1].is_ascii_digit()) {
            continue;
        }
        let j = i + 8 + (b.get(i + 8) == Some(&b'T')) as usize;
        let t = digits(j, 6).filter(|t| &t[..2] < "24" && &t[2..4] < "60").unwrap_or("000000");
        return parse(&format!("{d}T{t}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!((days(1970, 1, 1), days(2000, 3, 1), days(1969, 12, 31)), (0, 11017, -1));
        assert_eq!(parse("2000-01-01T00:00:00Z"), Some(946_684_800.0));
        assert_eq!(parse("2000-01-01"), Some(946_684_800.0));
        assert_eq!(parse("20000101T000130"), Some(946_684_890.0));
        assert_eq!(parse("2000-1-1 0:0:0.0"), Some(946_684_800.0));
        assert_eq!(parse("x"), None);
        for s in ["2026-09-22 10:27:01", "1999-12-31 23:59:59", "2024-02-29 00:00:00", "1960-03-01 12:00:00"] {
            assert_eq!(text(parse(s).unwrap()), s);
        }
        assert_eq!(cf("days since 2000-01-01 00:00:00"), Some((86400.0, 946_684_800.0)));
        assert_eq!(cf("seconds since 1970-01-01T00:00:00Z"), Some((1.0, 0.0)));
        assert_eq!(cf("hours since 1970-01-01"), Some((3600.0, 0.0)));
        assert_eq!(cf("K"), None);
        assert_eq!(in_name("S2A_MSIL2A_20260922T102701_N0513_R108").map(text).as_deref(), Some("2026-09-22 10:27:01"));
        assert_eq!(in_name("ESACCI-L4_20100615-fv2.nc").map(text).as_deref(), Some("2010-06-15 00:00:00"));
        assert_eq!(in_name("tile_123456789012.tif"), None);
    }
}
