//! Parsing and rendering of the durations used for the "not touched since"
//! threshold. Deliberately tiny: `s m h d w mo y`, combinable as `1y6mo`.

use std::time::{Duration, SystemTime};

use anyhow::{Result, bail};

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const WEEK: u64 = 7 * DAY;
const MONTH: u64 = 30 * DAY;
const YEAR: u64 = 365 * DAY;

/// `"30d"`, `"1y6mo"`, `"12h"`, `"90"` (days), `"0"`/`"off"`/`"any"` (disabled).
pub fn parse_duration(input: &str) -> Result<Duration> {
    let text = input.trim().to_ascii_lowercase();
    if text.is_empty() {
        bail!("empty duration");
    }
    if matches!(text.as_str(), "0" | "off" | "any" | "none") {
        return Ok(Duration::ZERO);
    }

    let mut total: u64 = 0;
    let mut number: Option<u64> = None;
    let mut unit = String::new();
    let mut saw_any = false;

    // Accumulate <digits><letters> pairs, flushing on every digit that follows
    // a unit so that "1y6mo" parses as two components.
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            if !unit.is_empty() {
                total += flush(number.take(), &unit, &text)?;
                unit.clear();
            }
            number = Some(
                number
                    .unwrap_or(0)
                    .checked_mul(10)
                    .and_then(|n| n.checked_add(u64::from(ch as u8 - b'0')))
                    .ok_or_else(|| anyhow::anyhow!("duration {input:?} is too large"))?,
            );
            saw_any = true;
        } else if ch.is_ascii_alphabetic() {
            unit.push(ch);
        } else if ch == ' ' || ch == '_' {
            continue;
        } else {
            bail!("unexpected character {ch:?} in duration {input:?}");
        }
    }
    if !saw_any {
        bail!("no number in duration {input:?}");
    }
    total += flush(number, &unit, &text)?;
    Ok(Duration::from_secs(total))
}

fn flush(number: Option<u64>, unit: &str, whole: &str) -> Result<u64> {
    let Some(number) = number else {
        if unit.is_empty() {
            return Ok(0);
        }
        bail!("unit {unit:?} without a number in {whole:?}");
    };
    let seconds = match unit {
        // A bare number is days: `--older-than 90` reads naturally.
        "" | "d" | "day" | "days" => DAY,
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => MINUTE,
        "h" | "hr" | "hrs" | "hour" | "hours" => HOUR,
        "w" | "week" | "weeks" => WEEK,
        "mo" | "mon" | "month" | "months" => MONTH,
        "y" | "yr" | "year" | "years" => YEAR,
        other => bail!("unknown duration unit {other:?} in {whole:?}"),
    };
    number
        .checked_mul(seconds)
        .ok_or_else(|| anyhow::anyhow!("duration {whole:?} is too large"))
}

/// `Duration` -> `"30d"`, `"6mo"`, `"1y"`: the inverse of [`parse_duration`]
/// for the values the UI cycles through.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs == 0 {
        return "off".to_string();
    }
    for (unit, size) in [
        ("y", YEAR),
        ("mo", MONTH),
        ("w", WEEK),
        ("d", DAY),
        ("h", HOUR),
    ] {
        if secs >= size && secs.is_multiple_of(size) {
            return format!("{}{unit}", secs / size);
        }
    }
    format!("{}s", secs)
}

/// Coarse "how long ago", e.g. `3 months`, `12 days`, `just now`.
pub fn format_age(age: Duration) -> String {
    let secs = age.as_secs();
    let plural = |n: u64, word: &str| {
        if n == 1 {
            format!("1 {word}")
        } else {
            format!("{n} {word}s")
        }
    };
    match secs {
        0..=59 => "just now".to_string(),
        s if s < HOUR => plural(s / MINUTE, "minute"),
        s if s < DAY => plural(s / HOUR, "hour"),
        s if s < WEEK * 2 => plural(s / DAY, "day"),
        s if s < MONTH * 2 => plural(s / WEEK, "week"),
        s if s < YEAR * 2 => plural(s / MONTH, "month"),
        s => plural(s / YEAR, "year"),
    }
}

/// Age of `t` relative to now, clamped at zero for timestamps in the future.
pub fn age_of(t: SystemTime) -> Duration {
    SystemTime::now()
        .duration_since(t)
        .unwrap_or(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units() {
        assert_eq!(
            parse_duration("30d").unwrap(),
            Duration::from_secs(30 * DAY)
        );
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90 * DAY));
        assert_eq!(
            parse_duration("12h").unwrap(),
            Duration::from_secs(12 * HOUR)
        );
        assert_eq!(parse_duration("2w").unwrap(), Duration::from_secs(2 * WEEK));
        assert_eq!(
            parse_duration("6mo").unwrap(),
            Duration::from_secs(6 * MONTH)
        );
        assert_eq!(parse_duration("0").unwrap(), Duration::ZERO);
        assert_eq!(parse_duration("off").unwrap(), Duration::ZERO);
    }

    #[test]
    fn parses_compound() {
        assert_eq!(
            parse_duration("1y6mo").unwrap(),
            Duration::from_secs(YEAR + 6 * MONTH)
        );
        assert_eq!(
            parse_duration("1w 3d").unwrap(),
            Duration::from_secs(WEEK + 3 * DAY)
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("d").is_err());
        assert!(parse_duration("10x").is_err());
        assert!(parse_duration("abc").is_err());
    }

    #[test]
    fn formats_duration_with_the_coarsest_exact_unit() {
        assert_eq!(format_duration(Duration::ZERO), "off");
        assert_eq!(format_duration(parse_duration("1y").unwrap()), "1y");
        assert_eq!(format_duration(parse_duration("6mo").unwrap()), "6mo");
        assert_eq!(format_duration(parse_duration("7d").unwrap()), "1w");
        assert_eq!(format_duration(parse_duration("10d").unwrap()), "10d");
    }

    #[test]
    fn formats_age() {
        assert_eq!(format_age(Duration::from_secs(30)), "just now");
        assert_eq!(format_age(Duration::from_secs(3 * DAY)), "3 days");
        assert_eq!(format_age(Duration::from_secs(5 * MONTH)), "5 months");
        assert_eq!(format_age(Duration::from_secs(3 * YEAR)), "3 years");
    }
}
