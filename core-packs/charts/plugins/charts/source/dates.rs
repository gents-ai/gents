//! ISO dates and times as seconds since the Unix epoch (UTC), calendar-aware
//! tick generation and a small strftime subset. Pure integer arithmetic: no
//! time zone database, no clock.

use crate::err::{Res, fail};

const DAY: i64 = 86_400;
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTHS_LONG: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date `z` days after 1970-01-01 as (year, month, day).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => 28 + u32::from(leap(y)),
    }
}

fn digits(s: &str, n: usize) -> Option<u32> {
    (s.len() == n && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
}

/// Parses `YYYY-MM-DD`, `YYYY-MM`, or a date with `T` or a space and
/// `HH:MM[:SS[.fraction]]` plus an optional `Z` or `+HH:MM` offset, into UTC
/// seconds. Returns `None` for anything else, including impossible dates.
pub fn parse(text: &str) -> Option<f64> {
    let s = text.trim();
    let (date, rest) = match s.find(['T', ' ']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let mut parts = date.split('-');
    let y = digits(parts.next()?, 4)?;
    let m = digits(parts.next()?, 2)?;
    let d = match parts.next() {
        Some(p) => digits(p, 2)?,
        None if rest.is_none() => 1,
        None => return None,
    };
    if parts.next().is_some() || !(1..=12).contains(&m) || y == 0 {
        return None;
    }
    if d == 0 || d > days_in_month(i64::from(y), m) {
        return None;
    }
    let mut secs = days_from_civil(i64::from(y), m, d) * DAY;
    if let Some(rest) = rest {
        secs += time_of_day(rest.trim())?;
    }
    Some(secs as f64)
}

fn time_of_day(rest: &str) -> Option<i64> {
    let (clock, offset) = match rest.find(['Z', 'z', '+', '-']) {
        Some(i) => (&rest[..i], Some(&rest[i..])),
        None => (rest, None),
    };
    let mut it = clock.split(':');
    let h = digits(it.next()?, 2)?;
    let mi = digits(it.next()?, 2)?;
    let (sec, frac_ok) = match it.next() {
        None => (0, true),
        Some(p) => {
            let (whole, frac) = match p.split_once('.') {
                Some((w, f)) => (w, Some(f)),
                None => (p, None),
            };
            let ok = frac.is_none_or(|f| {
                !f.is_empty() && f.len() <= 9 && f.bytes().all(|b| b.is_ascii_digit())
            });
            (digits(whole, 2)?, ok)
        }
    };
    if it.next().is_some() || !frac_ok || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    let mut t = i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(sec);
    if let Some(off) = offset
        && !off.eq_ignore_ascii_case("z")
    {
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let body = &off[1..];
        if !body.is_ascii() {
            return None;
        }
        let (oh, om) = match body.split_once(':') {
            Some((a, b)) => (digits(a, 2)?, digits(b, 2)?),
            None if body.len() == 4 => (digits(&body[..2], 2)?, digits(&body[2..], 2)?),
            None => (digits(body, 2)?, 0),
        };
        if oh > 23 || om > 59 {
            return None;
        }
        t -= sign * (i64::from(oh) * 3600 + i64::from(om) * 60);
    }
    Some(t)
}

fn split(t: f64) -> (i64, i64) {
    let s = t.floor() as i64;
    (s.div_euclid(DAY), s.rem_euclid(DAY))
}

/// Checks a strftime-style pattern: `%Y %y %m %d %H %M %S %b %B %%`.
pub fn validate_pattern(pattern: &str) -> Res<()> {
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('Y' | 'y' | 'm' | 'd' | 'H' | 'M' | 'S' | 'b' | 'B' | '%') => {}
                Some(other) => {
                    return fail(format!(
                        "the date format %{other} is not known; use %Y %m %d %H %M %S %b %B"
                    ));
                }
                None => {
                    return fail("the date format ends with a lone %; write %% for a percent sign");
                }
            }
        }
    }
    Ok(())
}

/// Formats `t` with `pattern`; unknown specifiers are written as they are.
pub fn format(t: f64, pattern: &str) -> String {
    let (days, secs) = split(t);
    let (y, m, d) = civil_from_days(days);
    let mut out = String::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('Y') => out.push_str(&format!("{y:04}")),
            Some('y') => out.push_str(&format!("{:02}", y.rem_euclid(100))),
            Some('m') => out.push_str(&format!("{m:02}")),
            Some('d') => out.push_str(&format!("{d:02}")),
            Some('H') => out.push_str(&format!("{:02}", secs / 3600)),
            Some('M') => out.push_str(&format!("{:02}", secs % 3600 / 60)),
            Some('S') => out.push_str(&format!("{:02}", secs % 60)),
            Some('b') => out.push_str(MONTHS[m as usize - 1]),
            Some('B') => out.push_str(MONTHS_LONG[m as usize - 1]),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Seconds(i64),
    Minutes(i64),
    Hours(i64),
    Days(i64),
    Weeks,
    Months(i64),
    Years(i64),
}

const STEPS: [(Unit, f64); 28] = [
    (Unit::Seconds(1), 1.0),
    (Unit::Seconds(5), 5.0),
    (Unit::Seconds(15), 15.0),
    (Unit::Seconds(30), 30.0),
    (Unit::Minutes(1), 60.0),
    (Unit::Minutes(5), 300.0),
    (Unit::Minutes(15), 900.0),
    (Unit::Minutes(30), 1800.0),
    (Unit::Hours(1), 3600.0),
    (Unit::Hours(3), 10_800.0),
    (Unit::Hours(6), 21_600.0),
    (Unit::Hours(12), 43_200.0),
    (Unit::Days(1), 86_400.0),
    (Unit::Days(2), 172_800.0),
    (Unit::Weeks, 604_800.0),
    (Unit::Months(1), 2_629_800.0),
    (Unit::Months(3), 7_889_400.0),
    (Unit::Months(6), 15_778_800.0),
    (Unit::Years(1), 31_557_600.0),
    (Unit::Years(2), 63_115_200.0),
    (Unit::Years(5), 157_788_000.0),
    (Unit::Years(10), 315_576_000.0),
    (Unit::Years(20), 631_152_000.0),
    (Unit::Years(50), 1_577_880_000.0),
    (Unit::Years(100), 3_155_760_000.0),
    (Unit::Years(200), 6_311_520_000.0),
    (Unit::Years(500), 15_778_800_000.0),
    (Unit::Years(1000), 31_557_600_000.0),
];

fn month_start(year: i64, month0: i64) -> i64 {
    let y = year + month0.div_euclid(12);
    days_from_civil(y, (month0.rem_euclid(12) + 1) as u32, 1) * DAY
}

fn floor_to(t: i64, unit: Unit) -> i64 {
    let fixed = |n: i64, secs: i64| t.div_euclid(n * secs) * n * secs;
    match unit {
        Unit::Seconds(n) => fixed(n, 1),
        Unit::Minutes(n) => fixed(n, 60),
        Unit::Hours(n) => fixed(n, 3600),
        Unit::Days(n) => fixed(n, DAY),
        Unit::Weeks => {
            let d = t.div_euclid(DAY);
            (d - (d + 3).rem_euclid(7)) * DAY
        }
        Unit::Months(n) => {
            let (y, m, _) = civil_from_days(t.div_euclid(DAY));
            let m0 = i64::from(m) - 1;
            month_start(y, m0 - m0.rem_euclid(n))
        }
        Unit::Years(n) => {
            let (y, _, _) = civil_from_days(t.div_euclid(DAY));
            month_start(y - y.rem_euclid(n), 0)
        }
    }
}

fn advance(t: i64, unit: Unit) -> i64 {
    match unit {
        Unit::Seconds(n) => t + n,
        Unit::Minutes(n) => t + n * 60,
        Unit::Hours(n) => t + n * 3600,
        Unit::Days(n) => t + n * DAY,
        Unit::Weeks => t + 7 * DAY,
        Unit::Months(n) => {
            let (y, m, _) = civil_from_days(t.div_euclid(DAY));
            month_start(y, i64::from(m) - 1 + n)
        }
        Unit::Years(n) => {
            let (y, _, _) = civil_from_days(t.div_euclid(DAY));
            month_start(y + n, 0)
        }
    }
}

/// Calendar ticks and their labels.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeTicks {
    /// Tick instants, ascending, in epoch seconds.
    pub values: Vec<f64>,
    /// One label per tick.
    pub labels: Vec<String>,
}

fn default_pattern(unit: Unit, first: f64, last: f64) -> &'static str {
    let one_year = civil_from_days(split(first).0).0 == civil_from_days(split(last).0).0;
    // Times alone are ambiguous once the range crosses midnight.
    let multi_day = last - first > 1.5 * DAY as f64 || split(first).0 != split(last).0;
    match unit {
        Unit::Years(_) => "%Y",
        Unit::Months(_) => "%b %Y",
        Unit::Days(_) | Unit::Weeks if one_year => "%b %d",
        Unit::Days(_) | Unit::Weeks => "%Y-%m-%d",
        Unit::Hours(_) | Unit::Minutes(_) if multi_day => "%b %d %H:%M",
        Unit::Hours(_) | Unit::Minutes(_) => "%H:%M",
        Unit::Seconds(_) => "%H:%M:%S",
    }
}

/// Ticks on calendar boundaries covering `[min, max]`: the first tick is at
/// or before `min` and the last at or after `max`. About `target` ticks.
pub fn ticks(min: f64, max: f64, target: usize, pattern: Option<&str>) -> TimeTicks {
    let max = if max > min { max } else { min + DAY as f64 };
    let target = target.max(2) as f64;
    let range = max - min;
    let mut unit = STEPS[0].0;
    let mut best = f64::INFINITY;
    for (u, secs) in STEPS {
        let ratio = range / secs / target;
        let score = if ratio >= 1.0 { ratio } else { 1.0 / ratio };
        if score < best {
            best = score;
            unit = u;
        }
    }
    let mut values = Vec::new();
    let mut t = floor_to(min.floor() as i64, unit);
    let end = max.ceil() as i64;
    while values.len() < 400 {
        values.push(t as f64);
        if t >= end {
            break;
        }
        t = advance(t, unit);
    }
    let pat = pattern.unwrap_or_else(|| default_pattern(unit, min, max));
    let labels = values.iter().map(|v| format(*v, pat)).collect();
    TimeTicks { values, labels }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> f64 {
        parse(s).unwrap_or_else(|| panic!("{s} should parse"))
    }

    #[test]
    fn the_epoch_and_known_dates_convert_exactly() {
        assert_eq!(p("1970-01-01"), 0.0);
        assert_eq!(p("1970-01-02"), 86_400.0);
        assert_eq!(p("2000-03-01"), 951_868_800.0);
        assert_eq!(p("2024-02-29"), 1_709_164_800.0);
        assert_eq!(p("1969-12-31"), -86_400.0);
        assert_eq!(p("2024-01-31T10:30:15Z"), 1_706_697_015.0);
    }

    #[test]
    fn civil_conversion_round_trips_over_a_wide_range() {
        for z in (-800_000..800_000).step_by(37) {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
            assert!((1..=12).contains(&m) && (1..=days_in_month(y, m)).contains(&d));
        }
    }

    #[test]
    fn times_with_offsets_and_fractions_shift_to_utc() {
        assert_eq!(p("2024-01-01 00:00"), p("2024-01-01"));
        assert_eq!(p("2024-01-01T12:00:00+02:00"), p("2024-01-01T10:00:00Z"));
        assert_eq!(p("2024-01-01T12:00:00-0530"), p("2024-01-01T17:30:00Z"));
        assert_eq!(p("2024-01-01T12:00:00+02"), p("2024-01-01T10:00:00Z"));
        assert_eq!(p("2024-01-01T00:00:00.250Z"), p("2024-01-01"));
        assert_eq!(p("2024-05"), p("2024-05-01"));
        assert_eq!(p("  2024-05-02  "), p("2024-05-02"));
    }

    #[test]
    fn impossible_or_malformed_dates_do_not_parse() {
        for bad in [
            "",
            "2024",
            "2024-13-01",
            "2024-00-10",
            "2024-02-30",
            "2023-02-29",
            "2024-04-31",
            "2024-1-01",
            "24-01-01",
            "2024-01-01T25:00",
            "2024-01-01T10:60",
            "2024-01-01T10:00:60",
            "2024-01-01T10",
            "2024-01-01Tabc",
            "0000-01-01",
            "2024/01/01",
            "2024-01-01T10:00+25:00",
            "2024-01-01T10:00:00.",
            "2024-01-01T10:00+a\u{e9}a",
            "2024-01-01x",
            "hello",
            "2024-01-01-05",
            "2024-01-01T10:00Zjunk",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn leap_years_follow_the_gregorian_rule() {
        assert!(parse("2000-02-29").is_some());
        assert!(parse("1900-02-29").is_none());
        assert!(parse("2100-02-29").is_none());
        assert!(parse("2024-02-29").is_some());
    }

    #[test]
    fn formatting_covers_every_specifier() {
        let t = p("2024-03-05T07:08:09Z");
        assert_eq!(format(t, "%Y-%m-%d %H:%M:%S"), "2024-03-05 07:08:09");
        assert_eq!(format(t, "%b %d, %y"), "Mar 05, 24");
        assert_eq!(format(t, "%B %Y 100%%"), "March 2024 100%");
        assert_eq!(format(t, "%q"), "%q");
        assert_eq!(
            format(p("1969-12-31T23:59:59Z"), "%Y-%m-%d %H:%M:%S"),
            "1969-12-31 23:59:59"
        );
    }

    #[test]
    fn format_patterns_are_validated_before_use() {
        assert!(validate_pattern("%Y-%m-%d %H:%M:%S %b %B %y %%").is_ok());
        assert!(validate_pattern("%q").unwrap_err().0.contains("%q"));
        assert!(validate_pattern("50%").is_err());
    }

    fn check_cover(min: &str, max: &str, target: usize) -> TimeTicks {
        let (a, b) = (p(min), p(max));
        let t = ticks(a, b, target, None);
        assert!(t.values.windows(2).all(|w| w[0] < w[1]), "ascending");
        assert!(t.values[0] <= a, "first tick covers the start");
        assert!(*t.values.last().unwrap() >= b, "last tick covers the end");
        assert_eq!(t.values.len(), t.labels.len());
        assert!(t.values.len() <= 12, "{} ticks", t.values.len());
        t
    }

    #[test]
    fn a_multi_year_range_ticks_on_january_firsts() {
        let t = check_cover("2015-06-10", "2024-03-01", 6);
        assert!(t.labels.iter().all(|l| l.len() == 4), "{:?}", t.labels);
        for v in &t.values {
            assert_eq!(format(*v, "%m-%d %H:%M:%S"), "01-01 00:00:00");
        }
    }

    #[test]
    fn a_months_long_range_ticks_on_month_starts_with_the_year() {
        let t = check_cover("2024-01-15", "2024-12-20", 6);
        for v in &t.values {
            assert_eq!(format(*v, "%d %H:%M"), "01 00:00");
        }
        assert!(t.labels[0].contains("2024"));
    }

    #[test]
    fn a_weeks_long_range_ticks_on_days_without_the_year() {
        let t = check_cover("2024-03-01", "2024-03-20", 6);
        assert_eq!(t.labels[0].len(), 6, "{:?}", t.labels);
        assert!(!t.labels[0].contains("2024"));
    }

    #[test]
    fn weekly_ticks_fall_on_mondays() {
        let t = ticks(p("2024-03-01"), p("2024-06-01"), 14, None);
        let weekday = |v: f64| (split(v).0 + 3).rem_euclid(7);
        assert!(t.values.windows(2).all(|w| w[1] - w[0] == 7.0 * 86_400.0));
        assert!(t.values.iter().all(|v| weekday(*v) == 0));
    }

    #[test]
    fn a_day_range_ticks_on_hours_and_shows_the_clock() {
        let t = check_cover("2024-03-01T08:00:00Z", "2024-03-01T20:00:00Z", 6);
        assert!(t.labels[0].contains(':'));
        assert!(!t.labels[0].contains("Mar"));
    }

    #[test]
    fn a_range_that_crosses_midnight_shows_the_date_with_the_clock() {
        let t = ticks(
            p("2024-05-01T00:00:00Z"),
            p("2024-05-02T00:00:00Z"),
            5,
            None,
        );
        assert_eq!(t.labels.first().map(String::as_str), Some("May 01 00:00"));
        assert_eq!(t.labels.last().map(String::as_str), Some("May 02 00:00"));
        let same_day = ticks(
            p("2024-05-01T08:00:00Z"),
            p("2024-05-01T20:00:00Z"),
            5,
            None,
        );
        assert!(!same_day.labels[0].contains("May"), "{:?}", same_day.labels);
    }

    #[test]
    fn a_minute_range_ticks_on_seconds() {
        let t = check_cover("2024-03-01T08:00:00Z", "2024-03-01T08:01:00Z", 6);
        assert_eq!(t.labels[0].matches(':').count(), 2);
    }

    #[test]
    fn a_user_pattern_replaces_the_default_label() {
        let t = ticks(p("2020-01-01"), p("2024-01-01"), 5, Some("'%y"));
        assert!(t.labels.iter().all(|l| l.starts_with('\'') && l.len() == 3));
    }

    #[test]
    fn a_degenerate_range_still_yields_covering_ticks() {
        let t = ticks(p("2024-03-01"), p("2024-03-01"), 5, None);
        assert!(t.values.len() >= 2);
        assert!(t.values[0] <= p("2024-03-01") && *t.values.last().unwrap() > p("2024-03-01"));
    }

    #[test]
    fn dates_before_the_epoch_tick_on_boundaries_too() {
        let t = check_cover("1950-03-01", "1969-07-01", 6);
        for v in &t.values {
            assert_eq!(format(*v, "%m-%d"), "01-01");
        }
    }

    #[test]
    fn a_huge_range_does_not_loop_forever() {
        let t = ticks(p("0001-01-01"), p("9999-12-31"), 6, None);
        assert!(t.values.len() <= 400);
    }
}
