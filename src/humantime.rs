//! RFC 3339 timestamp parsing and formatting for the Phase 4 timeline.
//!
//! The catalog stores timestamps as microseconds since the Unix epoch. A
//! time-range view needs a human interface for them, and time parsing is
//! exactly where silent correctness bugs live (month lengths, leap years,
//! offsets), so this module is deliberately small, dependency-free, and
//! exhaustively tested: the days-from-civil civil↔days conversion (proleptic
//! Gregorian) plus strict byte-driven field parsing. Output is always UTC.
//!
//! This module never reads the clock and never mutates anything; "now" is
//! the caller's concern, so behavior stays deterministic per invocation.

use crate::error::{Result, RewindError};

const MICROSECONDS_PER_SECOND: i64 = 1_000_000;
const MICROSECONDS_PER_DAY: i64 = 86_400 * MICROSECONDS_PER_SECOND;

/// Converts days since 1970-01-01 to a civil date (year, month, day) using
/// days-from-civil (Howard Hinnant's algorithm), proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u32, day as u32)
}

/// Inverse of [`civil_from_days`].
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = (y - era * 400) as u64;
    let mp = if month > 2 { month - 3 } else { month + 9 } as u64;
    let day_of_year = (153 * mp + 2) / 5 + day as u64 - 1;
    let day_of_era = 365 * year_of_era + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era as i64 - 719_468
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Parses an RFC 3339 timestamp into microseconds since the Unix epoch.
///
/// Accepted: `YYYY-MM-DDTHH:MM:SS(.fraction+)?(Z|±HH:MM)`. Lowercase
/// `t`/`z` and a space instead of `T` are accepted. Fractional seconds are
/// truncated (not rounded) to microseconds. A leap second (`23:59:60`) is
/// accepted and mapped deterministically to the following second.
pub fn parse_rfc3339(input: &str) -> Result<i64> {
    parse_rfc3339_impl(input).map_err(|detail| {
        RewindError::Storage(format!("invalid RFC 3339 timestamp {input:?}: {detail}"))
    })
}

/// Reads `count` ASCII digits starting at `position`. Byte-driven on
/// purpose: no string slicing, so non-UTF-8-shaped input can never panic.
fn digits(bytes: &[u8], position: usize, count: usize) -> Option<i64> {
    if position + count > bytes.len() {
        return None;
    }
    let mut value: i64 = 0;
    for &byte in &bytes[position..position + count] {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + (byte - b'0') as i64;
    }
    Some(value)
}

fn parse_rfc3339_impl(input: &str) -> std::result::Result<i64, String> {
    let bytes = input.as_bytes();
    let fail = |detail: &str| -> String { detail.to_string() };

    if bytes.len() < 20 {
        return Err(fail("timestamp too short"));
    }
    let year = digits(bytes, 0, 4).ok_or_else(|| fail("year must be four digits"))?;
    if bytes[4] != b'-' {
        return Err(fail("expected '-' after year"));
    }
    let month = digits(bytes, 5, 2).ok_or_else(|| fail("month must be two digits"))?;
    if bytes[7] != b'-' {
        return Err(fail("expected '-' after month"));
    }
    let day = digits(bytes, 8, 2).ok_or_else(|| fail("day must be two digits"))?;
    if bytes[10] != b'T' && bytes[10] != b't' && bytes[10] != b' ' {
        return Err(fail("expected 'T' between date and time"));
    }
    let hour = digits(bytes, 11, 2).ok_or_else(|| fail("hour must be two digits"))?;
    if bytes[13] != b':' {
        return Err(fail("expected ':' after hour"));
    }
    let minute = digits(bytes, 14, 2).ok_or_else(|| fail("minute must be two digits"))?;
    if bytes[16] != b':' {
        return Err(fail("expected ':' after minute"));
    }
    let second = digits(bytes, 17, 2).ok_or_else(|| fail("second must be two digits"))?;

    if !(1..=12).contains(&month) {
        return Err(fail("month out of range"));
    }
    if day < 1 || day > days_in_month(year, month as u32) as i64 {
        return Err(fail("day invalid for the given year and month"));
    }
    if hour > 23 || minute > 59 || second > 60 {
        return Err(fail("time of day out of range"));
    }
    if second == 60 && (hour != 23 || minute != 59) {
        // A leap second only exists at the end of a day: anything else is a
        // typo, and mapping it silently would invent an instant.
        return Err(fail("a leap second is only valid at 23:59:60"));
    }

    let mut position = 19;
    let mut fractional_micros: i64 = 0;
    if position < bytes.len() && bytes[position] == b'.' {
        position += 1;
        let start = position;
        while position < bytes.len() && bytes[position].is_ascii_digit() {
            position += 1;
        }
        if position == start {
            return Err(fail("fraction must have at least one digit"));
        }
        // Truncate to microseconds: keep the first six digits, ignore the
        // rest (truncation, never rounding, so parsing is idempotent).
        let mut value: i64 = 0;
        for &byte in &bytes[start..(start + 6).min(position)] {
            value = value * 10 + (byte - b'0') as i64;
        }
        let kept = (position - start).min(6) as u32;
        for _ in kept..6 {
            value *= 10;
        }
        fractional_micros = value;
    }

    if position >= bytes.len() {
        return Err(fail("missing UTC designator or offset"));
    }
    let offset_seconds: i64 = match bytes[position] {
        b'Z' | b'z' => {
            position += 1;
            0
        }
        sign @ (b'+' | b'-') => {
            let offset_hour = digits(bytes, position + 1, 2)
                .ok_or_else(|| fail("offset hour must be two digits"))?;
            if bytes.get(position + 3) != Some(&b':') {
                return Err(fail("offset must be ±HH:MM"));
            }
            let offset_minute = digits(bytes, position + 4, 2)
                .ok_or_else(|| fail("offset minute must be two digits"))?;
            if offset_hour > 23 || offset_minute > 59 {
                return Err(fail("offset out of range"));
            }
            let magnitude = offset_hour * 3600 + offset_minute * 60;
            position += 6;
            if sign == b'-' {
                -magnitude
            } else {
                magnitude
            }
        }
        other => return Err(format!("unexpected character {}", other as char)),
    };
    if position != bytes.len() {
        return Err(fail("trailing characters after timestamp"));
    }

    let (second_value, leap_adjustment) = if second == 60 {
        (59, MICROSECONDS_PER_SECOND)
    } else {
        (second, 0)
    };
    let days = days_from_civil(year, month as u32, day as u32);
    Ok(days * MICROSECONDS_PER_DAY
        + (hour * 3600 + minute * 60 + second_value) * MICROSECONDS_PER_SECOND
        + fractional_micros
        + leap_adjustment
        - offset_seconds * MICROSECONDS_PER_SECOND)
}

/// Formats microseconds since the Unix epoch as an RFC 3339 UTC timestamp
/// with microsecond precision (`YYYY-MM-DDTHH:MM:SS.ffffffZ`).
pub fn format_rfc3339(micros: i64) -> String {
    let days = micros.div_euclid(MICROSECONDS_PER_DAY);
    let seconds_of_day = micros.rem_euclid(MICROSECONDS_PER_DAY) / MICROSECONDS_PER_SECOND;
    let fractional = micros.rem_euclid(MICROSECONDS_PER_SECOND);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{fractional:06}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_known_instants_parse_exactly() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z").expect("epoch"), 0);
        // Verified against `date -u -d "2026-09-26T00:00:00Z" +%s`.
        assert_eq!(
            parse_rfc3339("2026-09-26T00:00:00Z").expect("known"),
            1_790_380_800_000_000
        );
        assert_eq!(
            format_rfc3339(1_790_380_800_000_000),
            "2026-09-26T00:00:00.000000Z"
        );
        // Verified against `date -u -d "2000-02-29T12:05:39Z" +%s`.
        assert_eq!(
            parse_rfc3339("2000-02-29T12:05:39Z").expect("leap day"),
            951_825_939_000_000
        );
        // Negative epochs (before 1970) format and parse correctly.
        // Verified against `date -u -d "1900-02-28T00:00:00Z" +%s`.
        let before_epoch = -2_203_977_600_000_000;
        assert_eq!(
            parse_rfc3339("1900-02-28T00:00:00Z").expect("pre-epoch"),
            before_epoch
        );
        assert_eq!(format_rfc3339(before_epoch), "1900-02-28T00:00:00.000000Z");
    }

    #[test]
    fn offsets_shift_into_the_utc_instant() {
        // 12:00+02:00 == 10:00Z.
        let utc = parse_rfc3339("2026-09-26T10:00:00Z").expect("utc");
        assert_eq!(
            parse_rfc3339("2026-09-26T12:00:00+02:00").expect("offset"),
            utc
        );
        // Negative offset crosses midnight backwards into the same instant.
        assert_eq!(
            parse_rfc3339("2026-01-01T00:30:00-01:00").expect("negative"),
            parse_rfc3339("2026-01-01T01:30:00Z").expect("same instant")
        );
        assert_eq!(
            parse_rfc3339("2026-09-26t10:00:00z").expect("lowercase"),
            utc
        );
        assert_eq!(parse_rfc3339("2026-09-26 10:00:00Z").expect("space"), utc);
        assert_eq!(
            parse_rfc3339("2026-09-26T10:00:00.123456789Z").expect("fraction"),
            utc + 123_456
        );
        // A leap second maps deterministically to the following second.
        assert_eq!(
            parse_rfc3339("2026-06-30T23:59:60Z").expect("leap second"),
            parse_rfc3339("2026-07-01T00:00:00Z").expect("next day")
        );
    }

    #[test]
    fn format_and_parse_round_trip_across_years() {
        // Every microsecond value constructed from valid field combinations
        // must format and parse back to itself.
        for &(year, month, day) in &[
            (1970, 1, 1),
            (1970, 1, 2),
            (2000, 2, 29), // leap year
            (1900, 2, 28), // century, not a leap year
            (2000, 12, 31),
            (2026, 9, 26),
            (2400, 2, 29),
            (9999, 12, 31),
        ] {
            let days = days_from_civil(year, month, day);
            for second_of_day in [0i64, 43_539, 86_399] {
                let micros =
                    days * MICROSECONDS_PER_DAY + second_of_day * MICROSECONDS_PER_SECOND + 7;
                let formatted = format_rfc3339(micros);
                assert_eq!(parse_rfc3339(&formatted).expect("round trip"), micros);
            }
        }
    }

    #[test]
    fn malformed_timestamps_are_rejected() {
        for bad in [
            "",
            "2026-09-26",
            "2026-09-26T10:00:00",       // no offset
            "2026-13-01T00:00:00Z",      // month out of range
            "2026-02-29T00:00:00Z",      // not a leap year
            "2026-09-31T00:00:00Z",      // day out of range
            "2026-09-26T24:00:00Z",      // hour out of range
            "2026-09-26T10:61:00Z",      // minute out of range
            "2026-09-26T10:00:60Z",      // leap second outside 23:59
            "2026-09-26T10:00:00+2400",  // offset missing colon
            "2026-09-26T10:00:00+02:60", // offset minute out of range
            "2026-09-26T10:00:00.Z",     // empty fraction
            "2026-09-26T10:00:00Zextra", // trailing garbage
            "2026/09/26T10:00:00Z",      // wrong separator
            "26-09-26T10:00:00Z",        // short year
            "été-09-26T10:00:00Z",       // non-ASCII input must not panic
        ] {
            assert!(
                parse_rfc3339(bad).is_err(),
                "{bad:?} must be rejected as a timestamp"
            );
        }
    }

    #[test]
    fn parse_error_messages_name_the_input() {
        let error = parse_rfc3339("not-a-time").expect_err("must fail");
        assert!(error.to_string().contains("not-a-time"));
    }
}
