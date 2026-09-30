//! Provider-reported usage of one provider account, normalized across the
//! passive channel (response headers) and the on-demand reads.
//!
//! Values are what the provider reported, never derived from local token
//! counts: a missing or malformed value drops its window, so "unknown" stays
//! unknown and is never shown as a percentage, as exhausted or as unlimited.
//! Window labels are identities: every source names the same window alike,
//! so [`UsageReport::merge`] joins a header window and an endpoint window.

use chrono::{DateTime, Datelike, Duration, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider_limit::{epoch_seconds, parse_rfc3339};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    /// Headers of a successful provider response.
    Header,
    /// An on-demand usage read.
    Endpoint,
    /// Headers of a rejected provider response.
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_minutes: Option<i64>,
    /// Percent of the window used, 0..=100.
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

impl UsageReport {
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty() && self.credits.is_none() && self.plan.is_none()
    }
}

/// Usage windows from a provider response's headers:
/// - Claude subscription `anthropic-ratelimit-unified-<window>-utilization`
///   (a 0..1 fraction) and `-reset` (epoch seconds), for `5h`, `7d` and
///   `7d_<model>`; overage headers are not usage windows.
/// - Codex `x-codex-{primary,secondary}-{used-percent,window-minutes,reset-at}`,
///   a window kept only when it carries a value, as Codex does
///   (codex-rs/codex-api/src/rate_limits.rs), and `x-codex-credits-*`.
/// - OpenAI API keys `x-ratelimit-{limit,remaining,reset}-{requests,tokens}`:
///   per-minute headroom, the reset a duration such as `6m0s`.
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
    report
}

/// ChatGPT `GET /wham/usage` (codex-rs/backend-client): `plan_type`,
/// `rate_limit.{primary,secondary}_window` and `credits`. `None` when the
/// body has no `rate_limit` object.
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

/// Grok `GET {cli-chat-proxy}/billing?format=credits` (xai-org/grok-build
/// `extensions/billing.rs`): one `period` window from
/// `creditUsagePercent` (0..100) and `currentPeriod`, under `config` or at the
/// top level, and `subscriptionTier` as the plan. Proto3 JSON omits a zero
/// percent, so a period without the field is 0 used. `None` for a body with
/// neither, such as the legacy `monthlyLimit`/`used` shape.
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

/// OpenRouter `GET /api/v1/key` (documented): a capped key is one window
/// named by `limit_reset` (`total` when it never resets), resetting at the
/// next UTC midnight, Monday or first of the month. An uncapped key gives an
/// empty report: the key has no cap, which says nothing about the account
/// balance. `None` without a `data` object.
pub fn openrouter_key(body: &Value, now: DateTime<Utc>) -> Option<UsageReport> {
    let data = body.get("data").filter(|value| value.is_object())?;
    let mut report = UsageReport::default();
    let limit = data
        .get("limit")
        .and_then(Value::as_f64)
        .filter(|limit| *limit > 0.0);
    let remaining = data.get("limit_remaining").and_then(Value::as_f64);
    if let Some(used) = limit
        .zip(remaining)
        .and_then(|(limit, remaining)| percent((limit - remaining) / limit * 100.0))
    {
        let reset = data.get("limit_reset").and_then(Value::as_str);
        report.windows.push(UsageWindow {
            label: reset.unwrap_or("total").to_string(),
            window_minutes: None,
            used_pct: used,
            resets_at: reset.and_then(|reset| next_openrouter_reset(reset, now)),
            source: UsageSource::Endpoint,
            observed_at: now,
        });
    }
    Some(report)
}

/// Claude `GET /api/oauth/usage`: `five_hour`, `seven_day` and
/// `seven_day_<model>`, each `{utilization, resets_at}` or null. Utilization
/// is read as a percent, as the open-source decoders do (unverified live).
/// `None` when the body is not an object.
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

/// One label per Claude window for headers (`5h`, `7d`, `7d_<model>`) and
/// the usage endpoint (`five_hour`, `seven_day`, `seven_day_<model>`).
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

/// A provider percent, clamped to 100; `None` for NaN or a negative value.
fn percent(value: f64) -> Option<f64> {
    (value.is_finite() && value >= 0.0).then_some(value.min(100.0))
}

/// An OpenAI reset duration: `1s`, `20ms`, `6m0s`, `1h2m3.5s`.
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
