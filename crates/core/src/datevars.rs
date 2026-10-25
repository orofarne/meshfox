//! Values of `type="date"` and `type="datetime"` variables/arguments (SPEC.md,
//! "Date and datetime variables").
//!
//! The stored form is always canonical: a `date` is `YYYY-MM-DD`, a
//! `datetime` is RFC3339 *with* an offset (`2026-10-25T14:30:00+03:00` or
//! `...Z`) -- the same shape as the `createdAt`/`updatedAt` stamps, so an
//! instant is never ambiguous. `parse_*` are strict (what `validate_value`
//! accepts); `normalize_*` are the lenient entry for interactive text
//! fields, turning what a person types (`2026-10-25 14:30`) into the
//! canonical form, assuming the local offset when none was typed.

use time::format_description::well_known::Rfc3339;
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

fn digits(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Strict `YYYY-MM-DD`; the day must exist (leap years, month lengths).
pub fn parse_date(s: &str) -> Option<Date> {
    if !s.is_ascii() || s.len() != 10 {
        return None;
    }
    let b = s.as_bytes();
    if b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let year = digits(&s[0..4])? as i32;
    let month = Month::try_from(digits(&s[5..7])? as u8).ok()?;
    let day = digits(&s[8..10])? as u8;
    Date::from_calendar_date(year, month, day).ok()
}

pub fn format_date(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

/// Strict RFC3339 with an offset (`T` separator, `Z` or `±HH:MM`).
pub fn parse_datetime(s: &str) -> Option<OffsetDateTime> {
    if !s.is_ascii() || s.contains(['t', 'z']) || s.as_bytes().get(10) != Some(&b'T') {
        return None;
    }
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

pub fn format_datetime(dt: OffsetDateTime) -> String {
    dt.format(&Rfc3339)
        .expect("an OffsetDateTime within the supported range always formats as RFC3339")
}

/// `+03:00` / `-05:30` / `Z`-less numeric form of `seconds` east of UTC.
pub fn format_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let abs = seconds.unsigned_abs();
    format!("{sign}{:02}:{:02}", abs / 3600, abs % 3600 / 60)
}

/// Parses `Z`, `±HH:MM`, `±HHMM` or `±HH` into seconds east of UTC.
pub fn parse_offset(s: &str) -> Option<i32> {
    if s == "Z" || s == "z" {
        return Some(0);
    }
    let sign = match s.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let rest = &s[1..];
    if !rest.is_ascii() {
        return None;
    }
    let (h, m) = match rest.len() {
        2 => (digits(rest)?, 0),
        4 => (digits(&rest[..2])?, digits(&rest[2..])?),
        5 if rest.as_bytes()[2] == b':' => (digits(&rest[..2])?, digits(&rest[3..])?),
        _ => return None,
    };
    if h > 23 || m > 59 {
        return None;
    }
    Some(sign * (h as i32 * 3600 + m as i32 * 60))
}

/// Seconds east of UTC that the machine's local time zone has at the Unix
/// instant `unix_ts` (0 where the platform can't tell).
pub fn local_offset_seconds(unix_ts: i64) -> i32 {
    #[cfg(unix)]
    {
        let t = unix_ts as libc::time_t;
        // SAFETY: `localtime_r` only writes into the `tm` we hand it.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return 0;
        }
        tm.tm_gmtoff as i32
    }
    #[cfg(not(unix))]
    {
        let _ = unix_ts;
        0
    }
}

/// The local offset in effect for the *wall-clock* time `wall` (DST-aware: the
/// offset is looked up twice, since the wall time itself isn't an instant yet).
pub fn local_offset_for_wall(wall: PrimitiveDateTime) -> UtcOffset {
    let as_utc = wall.assume_utc().unix_timestamp();
    let guess = local_offset_seconds(as_utc);
    let secs = local_offset_seconds(as_utc - i64::from(guess));
    UtcOffset::from_whole_seconds(secs).unwrap_or(UtcOffset::UTC)
}

/// Now, in the machine's local offset.
pub fn now_local() -> OffsetDateTime {
    let now = OffsetDateTime::now_utc();
    let secs = local_offset_seconds(now.unix_timestamp());
    let offset = UtcOffset::from_whole_seconds(secs).unwrap_or(UtcOffset::UTC);
    now.to_offset(offset)
}

pub fn today_local() -> Date {
    now_local().date()
}

fn parse_time_of_day(s: &str) -> Option<Time> {
    if !s.is_ascii() {
        return None;
    }
    let mut parts = s.split(':');
    let hour = digits(parts.next()?)?;
    let minute = digits(parts.next()?)?;
    let sec = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let (second, nano) = match sec {
        None => (0, 0),
        Some(sec) => match sec.split_once('.') {
            None => (digits(sec)?, 0),
            Some((whole, frac)) => {
                if frac.is_empty() || frac.len() > 9 {
                    return None;
                }
                let n = digits(frac)?;
                (digits(whole)?, n * 10u32.pow(9 - frac.len() as u32))
            }
        },
    };
    Time::from_hms_nano(hour as u8, minute as u8, second as u8, nano).ok()
}

/// Lenient counterpart of `parse_date`: trims, nothing else is accepted.
pub fn normalize_date_input(s: &str) -> Result<String, String> {
    let s = s.trim();
    parse_date(s)
        .map(format_date)
        .ok_or_else(|| format!("expected a date as YYYY-MM-DD, got {s:?}"))
}

/// Lenient counterpart of `parse_datetime` for text a person types: `T` or a
/// space between date and time, seconds optional, offset optional (`Z`,
/// `+03:00`, `+0300`, `+03`). With no offset the machine's local offset at
/// that wall time is used. Returns the canonical RFC3339 string.
pub fn normalize_datetime_input(s: &str) -> Result<String, String> {
    let s = s.trim();
    let err = || {
        format!("expected a date and time like 2026-10-25 14:30 or 2026-10-25T14:30:00+03:00, got {s:?}")
    };
    if parse_datetime(s).is_some() {
        return Ok(s.to_string());
    }
    if !s.is_ascii() || s.len() < 16 {
        return Err(err());
    }
    let date = parse_date(&s[..10]).ok_or_else(err)?;
    if !matches!(s.as_bytes()[10], b'T' | b't' | b' ') {
        return Err(err());
    }
    let rest = s[11..].trim();
    // The offset, if any, starts at the first `Z`/`+`/`-` after the time of day.
    let split = rest.find(['Z', 'z', '+', '-']);
    let (time_part, offset_part) = match split {
        Some(i) => (rest[..i].trim(), Some(rest[i..].trim())),
        None => (rest, None),
    };
    let time = parse_time_of_day(time_part).ok_or_else(err)?;
    let wall = PrimitiveDateTime::new(date, time);
    let offset = match offset_part {
        Some(o) => {
            let secs = parse_offset(o).ok_or_else(err)?;
            UtcOffset::from_whole_seconds(secs).map_err(|_| err())?
        }
        None => local_offset_for_wall(wall),
    };
    Ok(format_datetime(wall.assume_offset(offset)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_strict_and_real() {
        assert!(parse_date("2026-10-25").is_some());
        assert!(parse_date("2024-02-29").is_some());
        assert!(parse_date("2026-02-29").is_none());
        assert!(parse_date("2026-13-01").is_none());
        assert!(parse_date("2026-1-01").is_none());
        assert!(parse_date("26-10-2026").is_none());
        assert!(parse_date("2026/10/25").is_none());
        assert!(parse_date(" 2026-10-25").is_none());
        assert!(parse_date("2026-10-2é").is_none());
        assert!(parse_date("").is_none());
    }

    #[test]
    fn datetimes_need_an_offset_and_canonical_shape() {
        assert!(parse_datetime("2026-10-25T14:30:00+03:00").is_some());
        assert!(parse_datetime("2026-10-25T14:30:00Z").is_some());
        assert!(parse_datetime("2026-10-25T14:30:00.5-05:30").is_some());
        assert!(parse_datetime("2026-10-25T14:30:00").is_none());
        assert!(parse_datetime("2026-10-25 14:30:00+03:00").is_none());
        assert!(parse_datetime("2026-10-25t14:30:00Z").is_none());
        assert!(parse_datetime("2026-10-25T14:30:00z").is_none());
        assert!(parse_datetime("2026-10-25T24:00:00Z").is_none());
        assert!(parse_datetime("2026-02-30T10:00:00Z").is_none());
        assert!(parse_datetime("2026-10-25").is_none());
    }

    #[test]
    fn offsets_parse_and_format() {
        assert_eq!(parse_offset("Z"), Some(0));
        assert_eq!(parse_offset("+03:00"), Some(10800));
        assert_eq!(parse_offset("-0530"), Some(-19800));
        assert_eq!(parse_offset("+03"), Some(10800));
        assert_eq!(parse_offset("+24:00"), None);
        assert_eq!(parse_offset("03:00"), None);
        assert_eq!(format_offset(10800), "+03:00");
        assert_eq!(format_offset(-19800), "-05:30");
        assert_eq!(format_offset(0), "+00:00");
    }

    #[test]
    fn typed_input_is_normalized_to_rfc3339() {
        assert_eq!(
            normalize_datetime_input("2026-10-25 14:30+03:00").unwrap(),
            "2026-10-25T14:30:00+03:00"
        );
        assert_eq!(
            normalize_datetime_input(" 2026-10-25T14:30:15 +0300 ").unwrap(),
            "2026-10-25T14:30:15+03:00"
        );
        assert_eq!(
            normalize_datetime_input("2026-10-25 14:30Z").unwrap(),
            "2026-10-25T14:30:00Z"
        );
        assert_eq!(
            normalize_datetime_input("2026-10-25 14:30:00-05:30").unwrap(),
            "2026-10-25T14:30:00-05:30"
        );
        // Already canonical: untouched.
        assert_eq!(
            normalize_datetime_input("2026-10-25T14:30:00+03:00").unwrap(),
            "2026-10-25T14:30:00+03:00"
        );
        // No offset typed: the local one is attached, and the result is valid.
        let n = normalize_datetime_input("2026-10-25 14:30").unwrap();
        assert!(parse_datetime(&n).is_some(), "{n}");
        assert!(n.starts_with("2026-10-25T14:30:00"), "{n}");
        for bad in [
            "",
            "tomorrow",
            "2026-10-25",
            "2026-10-25 25:00",
            "2026-02-30 10:00",
            "2026-10-25 14:30+99:00",
            "2026-10-25 14",
        ] {
            assert!(normalize_datetime_input(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn local_wall_time_keeps_its_clock_digits() {
        let wall = PrimitiveDateTime::new(
            parse_date("2026-07-01").unwrap(),
            Time::from_hms(12, 0, 0).unwrap(),
        );
        let dt = wall.assume_offset(local_offset_for_wall(wall));
        assert_eq!((dt.hour(), dt.minute()), (12, 0));
    }

    #[test]
    fn date_input_normalizes_only_whitespace() {
        assert_eq!(normalize_date_input(" 2026-10-25 ").unwrap(), "2026-10-25");
        assert!(normalize_date_input("25.10.2026").is_err());
    }
}
