use std::collections::BTreeSet;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Datelike, Duration as ChronoDuration, Timelike, Utc};
use chrono_tz::Tz;

pub const DEFAULT_CRON_MISSED_RUN_POLICY: &str = "latest_only";

const MAX_CRON_LOOKAHEAD_MINUTES: i64 = 366 * 24 * 60 * 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CronMissedRunPolicy {
    LatestOnly,
}

impl CronMissedRunPolicy {
    pub fn parse(value: Option<&str>) -> Result<Self> {
        let value = value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_CRON_MISSED_RUN_POLICY);
        match value {
            DEFAULT_CRON_MISSED_RUN_POLICY => Ok(Self::LatestOnly),
            other => bail!(
                "unsupported missed_run_policy {other}; supported values: {DEFAULT_CRON_MISSED_RUN_POLICY}"
            ),
        }
    }
}

pub fn validate_cron_schedule(
    expression: &str,
    timezone: &str,
    missed_run_policy: Option<&str>,
) -> Result<()> {
    CronSpec::parse(expression)?;
    parse_timezone(timezone)?;
    CronMissedRunPolicy::parse(missed_run_policy)?;
    Ok(())
}

pub fn next_cron_run_after(
    expression: &str,
    timezone: &str,
    after_utc: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    let spec = CronSpec::parse(expression)?;
    let tz = parse_timezone(timezone)?;
    let mut candidate = truncate_to_next_minute(after_utc);

    for _ in 0..MAX_CRON_LOOKAHEAD_MINUTES {
        if spec.matches(candidate.with_timezone(&tz)) {
            return Ok(candidate);
        }
        candidate += ChronoDuration::minutes(1);
    }

    bail!(
        "cron expression {expression:?} in timezone {timezone:?} did not produce a run within 5 years"
    )
}

pub fn parse_timezone(timezone: &str) -> Result<Tz> {
    timezone
        .trim()
        .parse::<Tz>()
        .with_context(|| format!("invalid IANA timezone {timezone:?}"))
}

fn truncate_to_next_minute(after_utc: DateTime<Utc>) -> DateTime<Utc> {
    let next_minute = after_utc + ChronoDuration::minutes(1);
    next_minute
        .with_second(0)
        .and_then(|dt| dt.with_nanosecond(0))
        .expect("zero seconds and nanoseconds are valid")
}

#[derive(Debug, Clone)]
struct CronSpec {
    minutes: CronField,
    hours: CronField,
    days_of_month: CronField,
    months: CronField,
    days_of_week: CronField,
}

impl CronSpec {
    fn parse(expression: &str) -> Result<Self> {
        let fields = expression.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 5 {
            bail!(
                "cron expression must contain exactly 5 fields: minute hour day-of-month month day-of-week"
            );
        }

        Ok(Self {
            minutes: CronField::parse(fields[0], FieldKind::Minute)?,
            hours: CronField::parse(fields[1], FieldKind::Hour)?,
            days_of_month: CronField::parse(fields[2], FieldKind::DayOfMonth)?,
            months: CronField::parse(fields[3], FieldKind::Month)?,
            days_of_week: CronField::parse(fields[4], FieldKind::DayOfWeek)?,
        })
    }

    fn matches<TzRef: chrono::TimeZone>(&self, local: DateTime<TzRef>) -> bool {
        if !self.minutes.contains(local.minute()) {
            return false;
        }
        if !self.hours.contains(local.hour()) {
            return false;
        }
        if !self.months.contains(local.month()) {
            return false;
        }

        let dom_matches = self.days_of_month.contains(local.day());
        let dow_matches = self
            .days_of_week
            .contains(local.weekday().num_days_from_sunday());

        let day_matches = if !self.days_of_month.is_wildcard() && !self.days_of_week.is_wildcard() {
            dom_matches || dow_matches
        } else {
            dom_matches && dow_matches
        };

        day_matches
    }
}

#[derive(Debug, Clone)]
struct CronField {
    values: BTreeSet<u32>,
    wildcard: bool,
}

impl CronField {
    fn parse(raw: &str, kind: FieldKind) -> Result<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            bail!("cron {} field is empty", kind.label());
        }

        let mut values = BTreeSet::new();
        let mut wildcard = false;

        for part in raw.split(',') {
            let part = part.trim();
            if part.is_empty() {
                bail!("cron {} field contains an empty list item", kind.label());
            }
            if part.starts_with('*') {
                wildcard = true;
            }
            extend_part_values(part, kind, &mut values)?;
        }

        Ok(Self { values, wildcard })
    }

    fn contains(&self, value: u32) -> bool {
        self.values.contains(&value)
    }

    fn is_wildcard(&self) -> bool {
        self.wildcard
    }
}

fn extend_part_values(part: &str, kind: FieldKind, values: &mut BTreeSet<u32>) -> Result<()> {
    let (base, step, has_step) = match part.split_once('/') {
        Some((base, step)) => {
            let step = step.parse::<u32>().with_context(|| {
                format!("cron {} field has invalid step {step:?}", kind.label())
            })?;
            if step == 0 {
                bail!("cron {} field step must be greater than 0", kind.label());
            }
            (base, step, true)
        }
        None => (part, 1, false),
    };

    let (start, end) = if base == "*" {
        (kind.min(), kind.max())
    } else if let Some((start, end)) = base.split_once('-') {
        (kind.parse_value(start)?, kind.parse_value(end)?)
    } else {
        let value = kind.parse_value(base)?;
        if has_step {
            (value, kind.max())
        } else {
            (value, value)
        }
    };

    if start > end {
        bail!(
            "cron {} field range start {} is greater than end {}",
            kind.label(),
            start,
            end
        );
    }

    let mut value = start;
    while value <= end {
        values.insert(kind.normalize(value));
        match value.checked_add(step) {
            Some(next) => value = next,
            None => break,
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum FieldKind {
    Minute,
    Hour,
    DayOfMonth,
    Month,
    DayOfWeek,
}

impl FieldKind {
    fn label(self) -> &'static str {
        match self {
            Self::Minute => "minute",
            Self::Hour => "hour",
            Self::DayOfMonth => "day-of-month",
            Self::Month => "month",
            Self::DayOfWeek => "day-of-week",
        }
    }

    fn min(self) -> u32 {
        match self {
            Self::Minute => 0,
            Self::Hour => 0,
            Self::DayOfMonth => 1,
            Self::Month => 1,
            Self::DayOfWeek => 0,
        }
    }

    fn max(self) -> u32 {
        match self {
            Self::Minute => 59,
            Self::Hour => 23,
            Self::DayOfMonth => 31,
            Self::Month => 12,
            Self::DayOfWeek => 7,
        }
    }

    fn parse_value(self, raw: &str) -> Result<u32> {
        let raw = raw.trim();
        if raw.is_empty() {
            bail!("cron {} field contains an empty value", self.label());
        }

        let value = match self.named_value(raw) {
            Some(value) => value,
            None => raw.parse::<u32>().with_context(|| {
                format!("cron {} field has invalid value {raw:?}", self.label())
            })?,
        };

        if value < self.min() || value > self.max() {
            return Err(anyhow!(
                "cron {} field value {} is outside {}..={}",
                self.label(),
                value,
                self.min(),
                self.max()
            ));
        }

        Ok(value)
    }

    fn normalize(self, value: u32) -> u32 {
        match self {
            Self::DayOfWeek if value == 7 => 0,
            _ => value,
        }
    }

    fn named_value(self, raw: &str) -> Option<u32> {
        let upper = raw.to_ascii_uppercase();
        match self {
            Self::Month => match upper.as_str() {
                "JAN" => Some(1),
                "FEB" => Some(2),
                "MAR" => Some(3),
                "APR" => Some(4),
                "MAY" => Some(5),
                "JUN" => Some(6),
                "JUL" => Some(7),
                "AUG" => Some(8),
                "SEP" => Some(9),
                "OCT" => Some(10),
                "NOV" => Some(11),
                "DEC" => Some(12),
                _ => None,
            },
            Self::DayOfWeek => match upper.as_str() {
                "SUN" => Some(0),
                "MON" => Some(1),
                "TUE" => Some(2),
                "WED" => Some(3),
                "THU" => Some(4),
                "FRI" => Some(5),
                "SAT" => Some(6),
                _ => None,
            },
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn validates_five_field_cron_and_timezone() {
        validate_cron_schedule("30 3 * * MON", "America/Los_Angeles", Some("latest_only")).unwrap();
    }

    #[test]
    fn rejects_malformed_cron_expression() {
        let error = validate_cron_schedule("30 3 * *", "UTC", None).unwrap_err();
        assert!(
            error.to_string().contains("must contain exactly 5 fields"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_invalid_timezone() {
        let error = validate_cron_schedule("30 3 * * *", "Mars/Olympus", None).unwrap_err();
        assert!(
            error.to_string().contains("invalid IANA timezone"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn supports_single_value_steps() {
        let after = Utc.with_ymd_and_hms(2026, 1, 1, 0, 5, 0).unwrap();
        let next = next_cron_run_after("5/10 * * * *", "UTC", after).unwrap();

        assert_eq!(next, Utc.with_ymd_and_hms(2026, 1, 1, 0, 15, 0).unwrap());
    }

    #[test]
    fn computes_timezone_aware_next_run_crossing_local_day_boundary() {
        let after = Utc.with_ymd_and_hms(2026, 6, 1, 6, 50, 0).unwrap();
        let next = next_cron_run_after("30 0 * * *", "America/Los_Angeles", after).unwrap();

        assert_eq!(next, Utc.with_ymd_and_hms(2026, 6, 1, 7, 30, 0).unwrap());
        let local = next.with_timezone(&parse_timezone("America/Los_Angeles").unwrap());
        assert_eq!(local.day(), 1);
        assert_eq!(local.hour(), 0);
        assert_eq!(local.minute(), 30);
    }

    #[test]
    fn dst_spring_forward_skips_nonexistent_local_time() {
        // 2024-03-10 America/Los_Angeles: clocks jump 02:00 PST -> 03:00 PDT, so
        // local 02:30 never occurs that day. A `30 2 * * *` schedule must skip
        // 03-10 and fire on 03-11 at 02:30 PDT (= 09:30 UTC).
        let after = Utc.with_ymd_and_hms(2024, 3, 10, 0, 0, 0).unwrap();
        let next = next_cron_run_after("30 2 * * *", "America/Los_Angeles", after).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2024, 3, 11, 9, 30, 0).unwrap());
        let local = next.with_timezone(&parse_timezone("America/Los_Angeles").unwrap());
        assert_eq!((local.day(), local.hour(), local.minute()), (11, 2, 30));
    }

    #[test]
    fn dst_fall_back_fires_at_each_repeated_local_time() {
        // 2024-11-03 America/Los_Angeles: clocks fall 02:00 PDT -> 01:00 PST, so
        // local 01:30 occurs twice. The first call returns the earlier UTC
        // instant (01:30 PDT = 08:30 UTC); a follow-up call returns the second
        // occurrence (01:30 PST = 09:30 UTC) before moving to the next day.
        let after = Utc.with_ymd_and_hms(2024, 11, 3, 0, 0, 0).unwrap();
        let first = next_cron_run_after("30 1 * * *", "America/Los_Angeles", after).unwrap();
        assert_eq!(first, Utc.with_ymd_and_hms(2024, 11, 3, 8, 30, 0).unwrap());
        let second = next_cron_run_after("30 1 * * *", "America/Los_Angeles", first).unwrap();
        assert_eq!(second, Utc.with_ymd_and_hms(2024, 11, 3, 9, 30, 0).unwrap());
    }
}
