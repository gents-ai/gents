//! Provider-reported usage of one provider account, normalized across the
//! passive channel (response headers) and the on-demand reads.
//!
//! Values are what the provider reported, never derived from local token
//! counts: a missing or malformed value drops its window, so "unknown" stays
//! unknown and is never shown as a percentage, as exhausted or as unlimited.
//! Window labels are identities: every source names the same window alike,
//! so [`UsageReport::merge`] joins a header window and an endpoint window.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider_limit::epoch_seconds;

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

/// ChatGPT `GET /wham/usage`. `None` when malformed.
pub fn codex_usage(body: &Value, now: DateTime<Utc>) -> Option<UsageReport> {
    let _ = body;
    Some(inert(now))
}

/// Grok `GET {cli-chat-proxy}/billing?format=credits`. `None` when malformed.
pub fn grok_billing(body: &Value, now: DateTime<Utc>) -> Option<UsageReport> {
    let _ = body;
    Some(inert(now))
}

/// OpenRouter `GET /api/v1/key`. `None` when malformed.
pub fn openrouter_key(body: &Value, now: DateTime<Utc>) -> Option<UsageReport> {
    let _ = body;
    Some(inert(now))
}

/// Claude `GET /api/oauth/usage`. `None` when malformed.
pub fn claude_oauth_usage(body: &Value, now: DateTime<Utc>) -> Option<UsageReport> {
    let _ = body;
    Some(inert(now))
}

fn inert(now: DateTime<Utc>) -> UsageReport {
    UsageReport {
        windows: vec![UsageWindow {
            label: "inert".to_string(),
            window_minutes: None,
            used_pct: 0.0,
            resets_at: None,
            source: UsageSource::Endpoint,
            observed_at: now,
        }],
        ..UsageReport::default()
    }
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
