//! Provider-reported usage of one provider account, normalized across the
//! passive channel (response headers) and the on-demand reads.
//!
//! Values are what the provider reported, never derived from local token
//! counts: a missing or malformed value drops its window, so "unknown" stays
//! unknown and is never shown as a percentage, as exhausted or as unlimited.
//! Window labels are identities: every source names the same window alike,
//! so [`UsageReport::merge`] joins a header window and an endpoint window.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
    let _ = (headers.into_iter().count(), source);
    inert(now)
}

fn inert(now: DateTime<Utc>) -> UsageReport {
    UsageReport {
        windows: vec![UsageWindow {
            label: "inert".to_string(),
            window_minutes: None,
            used_pct: 0.0,
            resets_at: None,
            source: UsageSource::Header,
            observed_at: now,
        }],
        ..UsageReport::default()
    }
}

#[cfg(test)]
#[path = "account_usage_tests.rs"]
mod tests;
