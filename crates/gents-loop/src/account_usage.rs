use chrono::{DateTime, Datelike, Duration, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider_limit::{epoch_seconds, parse_rfc3339};

/// Usage observations become stale before they disappear so a last-known value
/// can remain visible without being presented as current.
pub const STALE_AFTER: Duration = Duration::minutes(15);
pub const LAST_KNOWN_FOR: Duration = Duration::minutes(60);
/// Throttles repeat reads triggered by opening usage details.
pub const READ_SKIP_WINDOW: Duration = Duration::minutes(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    Header,
    Endpoint,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_minutes: Option<i64>,
    pub used_pct: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<DateTime<Utc>>,
    pub source: UsageSource,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageCredits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_credits: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlimited: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<String>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsagePlan {
    pub name: String,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageReport {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<UsageWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<UsageCredits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<UsagePlan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Fresh,
    Stale,
}

impl UsageReport {
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty() && self.credits.is_none() && self.plan.is_none()
    }

    pub fn observed_at(&self) -> Option<DateTime<Utc>> {
        let windows = self.windows.iter().map(|window| window.observed_at);
        let credits = self.credits.iter().map(|credits| credits.observed_at);
        let plan = self.plan.iter().map(|plan| plan.observed_at);
        windows.chain(credits).chain(plan).max()
    }

    pub fn merge(self, other: UsageReport) -> UsageReport {
        let mut windows = self.windows;
        for window in other.windows {
            match windows.iter_mut().find(|kept| kept.label == window.label) {
                Some(kept) if window.observed_at >= kept.observed_at => *kept = window,
                Some(_) => {}
                None => windows.push(window),
            }
        }
        UsageReport {
            windows,
            credits: newer(self.credits, other.credits, |credits| credits.observed_at),
            plan: newer(self.plan, other.plan, |plan| plan.observed_at),
        }
    }

    pub fn same_values(&self, other: &UsageReport) -> bool {
        self.values() == other.values()
    }

    fn values(&self) -> UsageReport {
        let epoch = DateTime::<Utc>::UNIX_EPOCH;
        let mut report = self.clone();
        for window in &mut report.windows {
            window.observed_at = epoch;
        }
        if let Some(credits) = &mut report.credits {
            credits.observed_at = epoch;
        }
        if let Some(plan) = &mut report.plan {
            plan.observed_at = epoch;
        }
        report
    }
}

fn newer<T>(kept: Option<T>, other: Option<T>, at: fn(&T) -> DateTime<Utc>) -> Option<T> {
    match (kept, other) {
        (Some(kept), Some(other)) if at(&kept) > at(&other) => Some(kept),
        (kept, other) => other.or(kept),
    }
}

pub fn visible_windows(report: &UsageReport, now: DateTime<Utc>) -> Vec<(&UsageWindow, Freshness)> {
    report
        .windows
        .iter()
        .filter(|window| window.resets_at.is_none_or(|reset| reset > now))
        .filter_map(|window| {
            let age = now - window.observed_at;
            if age > LAST_KNOWN_FOR {
                None
            } else if age > STALE_AFTER {
                Some((window, Freshness::Stale))
            } else {
                Some((window, Freshness::Fresh))
            }
        })
        .collect()
}

/// Anthropic API-key bucket reset headers are RFC 3339 timestamps
/// (<https://platform.claude.com/docs/en/api/rate-limits>); OpenRouter's
/// unqualified X-RateLimit-Reset is epoch milliseconds
/// (<https://openrouter.ai/docs/api_reference/limits>).
pub fn usage_from_headers<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
    source: UsageSource,
    now: DateTime<Utc>,
) -> UsageReport {
    let headers: Vec<(String, &str)> = headers
        .into_iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim()))
        .collect();
    let get = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| *value)
    };
    let window = |label: String, used_pct, window_minutes, resets_at| UsageWindow {
        label,
        window_minutes,
        used_pct,
        resets_at,
        source,
        observed_at: now,
    };
    let mut report = UsageReport::default();

    const UNIFIED: &str = "anthropic-ratelimit-unified-";
    for (name, value) in &headers {
        let Some(segment) = name
            .strip_prefix(UNIFIED)
            .and_then(|rest| rest.strip_suffix("-utilization"))
        else {
            continue;
        };
        let (Some(label), Some(used)) = (
            claude_label(segment),
            value.parse::<f64>().ok().and_then(|x| percent(x * 100.0)),
        ) else {
            continue;
        };
        let reset = get(&format!("{UNIFIED}{segment}-reset")).and_then(epoch_seconds);
        report.windows.push(window(label, used, None, reset));
    }

    for slot in ["primary", "secondary"] {
        let Some(used) = get(&format!("x-codex-{slot}-used-percent"))
            .and_then(|value| value.parse::<f64>().ok())
            .and_then(percent)
        else {
            continue;
        };
        let minutes = get(&format!("x-codex-{slot}-window-minutes"))
            .and_then(|value| value.parse::<i64>().ok());
        let reset = get(&format!("x-codex-{slot}-reset-at")).and_then(epoch_seconds);
        if used != 0.0 || minutes.is_some_and(|minutes| minutes != 0) || reset.is_some() {
            report
                .windows
                .push(window(slot.to_string(), used, minutes, reset));
        }
    }
    let flag = |name: &str| get(name).and_then(|value| value.parse::<bool>().ok());
    let credits = UsageCredits {
        has_credits: flag("x-codex-credits-has-credits"),
        unlimited: flag("x-codex-credits-unlimited"),
        balance: get("x-codex-credits-balance").map(str::to_string),
        observed_at: now,
    };
    if credits.has_credits.is_some() || credits.unlimited.is_some() || credits.balance.is_some() {
        report.credits = Some(credits);
    }

    for kind in ["requests", "tokens"] {
        let number = |name: &str| {
            get(&format!("x-ratelimit-{name}-{kind}")).and_then(|value| value.parse::<f64>().ok())
        };
        let (Some(limit), Some(remaining)) = (number("limit"), number("remaining")) else {
            continue;
        };
        if limit <= 0.0 {
            continue;
        }
        let Some(used) = percent((limit - remaining) / limit * 100.0) else {
            continue;
        };
        let reset = get(&format!("x-ratelimit-reset-{kind}"))
            .and_then(parse_duration)
            .and_then(|delay| now.checked_add_signed(delay));
        report
            .windows
            .push(window(kind.to_string(), used, None, reset));
    }

    for kind in ["requests", "tokens", "input-tokens", "output-tokens"] {
        let number = |suffix: &str| {
            get(&format!("anthropic-ratelimit-{kind}-{suffix}"))
                .and_then(|value| value.parse::<f64>().ok())
        };
        if let (Some(limit), Some(remaining)) = (number("limit"), number("remaining")) {
            if limit > 0.0 {
                if let Some(used) = percent((limit - remaining) / limit * 100.0) {
                    let reset =
                        get(&format!("anthropic-ratelimit-{kind}-reset")).and_then(parse_rfc3339);
                    report.windows.push(window(kind.into(), used, None, reset));
                }
            }
        }
    }
    if let (Some(limit), Some(remaining)) = (
        get("x-ratelimit-limit").and_then(|value| value.parse::<f64>().ok()),
        get("x-ratelimit-remaining").and_then(|value| value.parse::<f64>().ok()),
    ) {
        if limit > 0.0 {
            if let Some(used) = percent((limit - remaining) / limit * 100.0) {
                let reset = get("x-ratelimit-reset")
                    .and_then(|value| value.parse::<i64>().ok())
                    .and_then(DateTime::from_timestamp_millis);
                report
                    .windows
                    .push(window("requests".into(), used, None, reset));
            }
        }
    }
    report
}

pub fn codex_usage(body: &Value, now: DateTime<Utc>) -> Option<UsageReport> {
    let rate_limit = body.get("rate_limit").filter(|value| value.is_object())?;
    let mut report = UsageReport::default();
    for slot in ["primary", "secondary"] {
        let Some(window) = rate_limit.get(format!("{slot}_window")) else {
            continue;
        };
        let Some(used) = window
            .get("used_percent")
            .and_then(Value::as_f64)
            .and_then(percent)
        else {
            continue;
        };
        report.windows.push(UsageWindow {
            label: slot.to_string(),
            window_minutes: window
                .get("limit_window_seconds")
                .and_then(Value::as_i64)
                .map(|seconds| seconds / 60),
            used_pct: used,
            resets_at: window
                .get("reset_at")
                .and_then(Value::as_i64)
                .and_then(|seconds| epoch_seconds(&seconds.to_string())),
            source: UsageSource::Endpoint,
            observed_at: now,
        });
    }
    report.plan = plan(body.get("plan_type"), now);
    if let Some(credits) = body.get("credits").filter(|value| value.is_object()) {
        report.credits = Some(UsageCredits {
            has_credits: credits.get("has_credits").and_then(Value::as_bool),
            unlimited: credits.get("unlimited").and_then(Value::as_bool),
            balance: credits.get("balance").and_then(|balance| match balance {
                Value::String(text) => Some(text.clone()),
                Value::Number(number) => Some(number.to_string()),
                _ => None,
            }),
            observed_at: now,
        });
    }
    Some(report)
}

pub fn grok_billing(body: &Value, now: DateTime<Utc>) -> Option<UsageReport> {
    let config = body
        .get("config")
        .filter(|value| value.is_object())
        .unwrap_or(body);
    let period = config
        .get("currentPeriod")
        .filter(|value| value.is_object());
    let used = match config.get("creditUsagePercent") {
        Some(value) => percent(value.as_f64()?)?,
        None if period.is_some() => 0.0,
        None => return None,
    };
    let time = |name: &str| {
        period
            .and_then(|period| period.get(name))
            .and_then(Value::as_str)
            .and_then(parse_rfc3339)
    };
    let (start, end) = (time("start"), time("end"));
    Some(UsageReport {
        windows: vec![UsageWindow {
            label: "period".to_string(),
            window_minutes: start
                .zip(end)
                .map(|(start, end)| (end - start).num_minutes()),
            used_pct: used,
            resets_at: end,
            source: UsageSource::Endpoint,
            observed_at: now,
        }],
        credits: None,
        plan: plan(body.get("subscriptionTier"), now),
    })
}

pub fn openrouter_key(body: &Value, now: DateTime<Utc>) -> Option<UsageReport> {
    let data = body.get("data").filter(|value| value.is_object())?;
    let mut report = UsageReport::default();
    let limit = data.get("limit")?;
    if limit.is_null() {
        return Some(report);
    }
    let limit = limit
        .as_f64()
        .filter(|limit| limit.is_finite() && *limit > 0.0)?;
    let remaining = data
        .get("limit_remaining")?
        .as_f64()
        .filter(|value| value.is_finite())?;
    let used = percent((limit - remaining) / limit * 100.0)?;
    let reset = data.get("limit_reset").and_then(Value::as_str);
    report.windows.push(UsageWindow {
        label: reset.unwrap_or("total").to_string(),
        window_minutes: None,
        used_pct: used,
        resets_at: reset.and_then(|reset| next_openrouter_reset(reset, now)),
        source: UsageSource::Endpoint,
        observed_at: now,
    });
    Some(report)
}

pub fn claude_oauth_usage(body: &Value, now: DateTime<Utc>) -> Option<UsageReport> {
    let mut report = UsageReport::default();
    for (key, window) in body.as_object()? {
        let Some(label) = claude_label(key) else {
            continue;
        };
        let Some(used) = window
            .get("utilization")
            .and_then(Value::as_f64)
            .and_then(percent)
        else {
            continue;
        };
        report.windows.push(UsageWindow {
            label,
            window_minutes: None,
            used_pct: used,
            resets_at: window
                .get("resets_at")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339),
            source: UsageSource::Endpoint,
            observed_at: now,
        });
    }
    Some(report)
}

fn claude_label(segment: &str) -> Option<String> {
    match segment {
        "5h" | "five_hour" => Some("5h".to_string()),
        "7d" | "seven_day" => Some("7d".to_string()),
        _ => segment
            .strip_prefix("7d_")
            .or_else(|| segment.strip_prefix("seven_day_"))
            .filter(|model| !model.is_empty())
            .map(|model| format!("7d {model}")),
    }
}

fn plan(value: Option<&Value>, now: DateTime<Utc>) -> Option<UsagePlan> {
    let name = value?.as_str().filter(|name| !name.is_empty())?;
    Some(UsagePlan {
        name: name.to_string(),
        observed_at: now,
    })
}

fn next_openrouter_reset(reset: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let today = now.date_naive();
    let date = match reset {
        "daily" => today.succ_opt()?,
        "weekly" => {
            today + chrono::Days::new(7 - u64::from(today.weekday().num_days_from_monday()))
        }
        "monthly" => today
            .with_day(1)?
            .checked_add_months(chrono::Months::new(1))?,
        _ => return None,
    };
    Some(Utc.from_utc_datetime(&date.and_time(NaiveTime::MIN)))
}

fn percent(value: f64) -> Option<f64> {
    (value.is_finite() && value >= 0.0).then_some(value.min(100.0))
}

fn parse_duration(value: &str) -> Option<Duration> {
    let mut rest = value;
    let mut millis = 0.0;
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        let amount: f64 = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
        let unit = rest
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(rest.len());
        millis += amount
            * match &rest[..unit] {
                "h" => 3_600_000.0,
                "m" => 60_000.0,
                "s" => 1_000.0,
                "ms" => 1.0,
                _ => return None,
            };
        rest = &rest[unit..];
    }
    (millis.is_finite() && !value.is_empty())
        .then(|| Duration::try_milliseconds(millis.ceil() as i64))
        .flatten()
}

#[cfg(test)]
#[path = "account_usage_tests.rs"]
mod tests;
