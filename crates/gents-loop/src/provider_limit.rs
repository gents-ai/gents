//! Provider rate-limit classification shared by the owned loop's retry
//! boundary and the Goal usage-limit transition.
//!
//! A rejected provider call is either a throttle, which clears after a short
//! wait and belongs on the transport ladder, or an exhausted usage or quota
//! window, which cannot succeed before its reset and must fail the request
//! with that reset time. Only error text survives Rig's error boundary, so the
//! native transport appends the relevant response headers to the rejected
//! body as a marker ([`ProviderLimitHeaders::marker`]) and this classifier
//! reads the marker and the provider body together.

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};

/// A requested wait beyond this is a usage window, not a throttle: sleeping
/// through it would hold the request for the window instead of reporting it.
/// Matches oh-my-pi's `LONG_RATE_LIMIT_DELAY_MS` (packages/ai/src/error/rate-limit.ts).
pub const LONG_RATE_LIMIT_WAIT: Duration = Duration::from_secs(5 * 60);

const MARKER_OPEN: &str = " [provider-limit";
const USAGE_LIMIT_PREFIX: &str = "provider usage limit reached";
const DETAIL_MAX_CHARS: usize = 300;

/// Case-insensitive body evidence of an exhausted account window, by source:
/// - Codex: `usage_limit_reached` and `usage_not_included` error types
///   (codex-rs/codex-api/src/api_bridge.rs), `insufficient_quota` code
///   (codex-rs/codex-api/src/sse/responses.rs `is_quota_exceeded_error`),
///   and OpenAI's "exceeded your current quota" message for that code.
/// - xAI: `subscription:free-usage-exhausted` on a 429
///   (grok-build xai-grok-pager/src/app/dispatch/billing.rs).
/// - Anthropic subscription: "exceed your account's rate limit" on a 429
///   `rate_limit_error` while the seat is capped (#1422 live repro), the same
///   account-scoped wording oh-my-pi's `ACCOUNT_RATE_LIMIT_PATTERN` treats as
///   quota exhaustion; and "credit balance" billing rejections.
const USAGE_NEEDLES: &[&str] = &[
    "usage limit",
    "usage_limit",
    "usage_not_included",
    "insufficient_quota",
    "exceeded your current quota",
    "quota exceeded",
    "free-usage-exhausted",
    "account's rate limit",
    "billing hard limit",
    "credit balance",
];

const THROTTLE_NEEDLES: &[&str] = &["rate_limit", "rate limit", "too many requests"];

/// Status renderings of Rig's `InvalidStatusCodeWithMessage` and the Claude
/// Messages error, each followed by the code.
const STATUS_PREFIXES: &[&str] = &[
    "invalid status code ",
    "invalid status code: ",
    "invalidstatuscodewithmessage(",
    "status code ",
    "http status ",
    "http ",
];

/// Rate-limit facts read from a rejected response's headers, as absolute
/// times so a later reader (the Goal) interprets them against the response
/// rather than its own clock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderLimitHeaders {
    /// Earliest retry the provider allows: `retry-after-ms`, then
    /// `retry-after` (delta-seconds or HTTP-date).
    pub retry_at: Option<DateTime<Utc>>,
    /// Latest reset among the exhausted usage windows.
    pub reset_at: Option<DateTime<Utc>>,
    /// A usage window, not a throttle, rejected the request.
    pub usage_exhausted: bool,
}

impl ProviderLimitHeaders {
    /// Reads, besides `retry-after`:
    /// - Anthropic subscription unified limits: `anthropic-ratelimit-unified-status`
    ///   and per-window `anthropic-ratelimit-unified-<window>-status` equal to
    ///   `rejected`, each with a sibling `-reset` in epoch seconds.
    /// - Codex: `x-codex-{primary,secondary}-used-percent` at or above 100 with
    ///   `x-codex-{primary,secondary}-reset-at` in epoch seconds
    ///   (codex-rs/codex-api/src/rate_limits.rs `parse_rate_limit_window`).
    pub fn from_headers<'a>(
        headers: impl IntoIterator<Item = (&'a str, &'a str)>,
        now: DateTime<Utc>,
    ) -> Self {
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

        let retry_at = get("retry-after-ms")
            .and_then(|value| value.parse::<f64>().ok())
            .and_then(|ms| delay_after(now, ms / 1000.0))
            .or_else(|| get("retry-after").and_then(|value| parse_retry_after(value, now)));

        let mut limits = Self {
            retry_at,
            ..Self::default()
        };
        const UNIFIED: &str = "anthropic-ratelimit-unified-";
        for (name, value) in &headers {
            let Some(window) = name
                .strip_prefix(UNIFIED)
                .and_then(|rest| rest.strip_suffix("status"))
            else {
                continue;
            };
            if value.eq_ignore_ascii_case("rejected") {
                let reset = get(&format!("{UNIFIED}{window}reset")).and_then(epoch_seconds);
                limits.exhaust(reset);
            }
        }
        for window in ["primary", "secondary"] {
            let used = get(&format!("x-codex-{window}-used-percent"))
                .and_then(|value| value.parse::<f64>().ok());
            if used.is_some_and(|used| used >= 100.0) {
                let reset = get(&format!("x-codex-{window}-reset-at")).and_then(epoch_seconds);
                limits.exhaust(reset);
            }
        }
        limits
    }

    fn exhaust(&mut self, reset: Option<DateTime<Utc>>) {
        self.usage_exhausted = true;
        self.reset_at = self.reset_at.max(reset);
    }

    /// Suffix for a rejected response body; empty when no header applied.
    pub fn marker(&self) -> String {
        if *self == Self::default() {
            return String::new();
        }
        let mut marker = String::from(MARKER_OPEN);
        if let Some(at) = self.retry_at {
            marker.push_str(&format!(
                " retry-at={}",
                at.to_rfc3339_opts(SecondsFormat::AutoSi, true)
            ));
        }
        if let Some(at) = self.reset_at {
            marker.push_str(&format!(" reset-at={}", rfc3339(at)));
        }
        if self.usage_exhausted {
            marker.push_str(" usage-exhausted");
        }
        marker.push(']');
        marker
    }

    fn parse_marker(marker: &str) -> Self {
        let fields = marker
            .strip_prefix(MARKER_OPEN)
            .and_then(|rest| rest.split(']').next())
            .unwrap_or_default();
        let mut limits = Self::default();
        for field in fields.split_whitespace() {
            match field.split_once('=') {
                Some(("retry-at", value)) => limits.retry_at = parse_rfc3339(value),
                Some(("reset-at", value)) => limits.reset_at = parse_rfc3339(value),
                None if field == "usage-exhausted" => limits.usage_exhausted = true,
                _ => {}
            }
        }
        limits
    }
}

/// Splits `text` into the provider body and its header marker (empty when
/// absent), so a caller that bounds the body can reattach the marker intact.
pub fn split_provider_limit_marker(text: &str) -> (&str, &str) {
    match text.rfind(MARKER_OPEN) {
        Some(start) => (&text[..start], &text[start..]),
        None => (text, ""),
    }
}

/// An exhausted provider usage window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageLimit {
    pub resets_at: Option<DateTime<Utc>>,
    /// The provider's own explanation, bounded.
    pub detail: String,
}

impl fmt::Display for UsageLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.resets_at {
            Some(at) => write!(f, "{USAGE_LIMIT_PREFIX} (resets at {})", rfc3339(at))?,
            None => write!(f, "{USAGE_LIMIT_PREFIX} (reset time not reported)")?,
        }
        if !self.detail.is_empty() {
            write!(f, ": {}", self.detail)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderLimit {
    /// Retry after the hint, or the ladder delay when none was given.
    Throttled { retry_after: Option<Duration> },
    /// Retrying before the reset cannot succeed.
    UsageExhausted(UsageLimit),
}

/// Classifies a provider error text; `None` when it is not a rate limit.
/// A bare 429 is a throttle: only usage evidence (body wording, rejected usage
/// headers, or a wait beyond [`LONG_RATE_LIMIT_WAIT`]) makes it a usage limit.
pub fn classify_provider_limit(text: &str, now: DateTime<Utc>) -> Option<ProviderLimit> {
    let (body, marker) = split_provider_limit_marker(text);
    let headers = ProviderLimitHeaders::parse_marker(marker);
    let lower = body.to_ascii_lowercase();
    let usage_text = USAGE_NEEDLES.iter().any(|needle| lower.contains(needle));
    let throttle_text =
        THROTTLE_NEEDLES.iter().any(|needle| lower.contains(needle)) || has_status_429(&lower);
    if !(usage_text || throttle_text || headers.usage_exhausted) {
        return None;
    }

    let body_json = embedded_json(body);
    if usage_text || headers.usage_exhausted {
        let resets_at = body_json
            .as_ref()
            .and_then(|json| json_resets_at(json, now))
            .or_else(|| rendered_resets_at(body))
            .or(headers.reset_at)
            .or(headers.retry_at);
        return Some(ProviderLimit::UsageExhausted(UsageLimit {
            resets_at,
            detail: provider_detail(body, body_json.as_ref()),
        }));
    }

    let retry_at = headers
        .retry_at
        .or_else(|| try_again_in(&lower).and_then(|wait| delay_after(now, wait.as_secs_f64())));
    let retry_after = retry_at.map(|at| (at - now).to_std().unwrap_or(Duration::ZERO));
    if retry_after.is_some_and(|wait| wait > LONG_RATE_LIMIT_WAIT) {
        return Some(ProviderLimit::UsageExhausted(UsageLimit {
            resets_at: retry_at,
            detail: provider_detail(body, body_json.as_ref()),
        }));
    }
    Some(ProviderLimit::Throttled { retry_after })
}

fn has_status_429(lower: &str) -> bool {
    STATUS_PREFIXES.iter().any(|prefix| {
        lower.match_indices(prefix).any(|(at, _)| {
            let rest = &lower[at + prefix.len()..];
            rest.starts_with("429") && !rest[3..].starts_with(|c: char| c.is_ascii_digit())
        })
    })
}

/// The first `{` through the last `}` of `text`, when that parses as JSON.
fn embedded_json(text: &str) -> Option<serde_json::Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (start < end)
        .then(|| serde_json::from_str(&text[start..=end]).ok())
        .flatten()
}

/// Codex `error.resets_at` (epoch seconds) or `error.resets_in_seconds`.
fn json_resets_at(json: &serde_json::Value, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let error = json.get("error").unwrap_or(json);
    if let Some(at) = error.get("resets_at").and_then(serde_json::Value::as_i64) {
        return Utc.timestamp_opt(at, 0).single();
    }
    error
        .get("resets_in_seconds")
        .and_then(serde_json::Value::as_f64)
        .and_then(|seconds| delay_after(now, seconds))
}

/// Re-reads a [`UsageLimit`] rendering, so classification is idempotent over
/// the terminal error it produced.
fn rendered_resets_at(body: &str) -> Option<DateTime<Utc>> {
    let start = body.find("(resets at ")? + "(resets at ".len();
    let end = body[start..].find(')')? + start;
    parse_rfc3339(&body[start..end])
}

/// `error.message`, xAI's flat `error`, or a top-level `message`; otherwise the
/// trimmed text.
fn provider_detail(body: &str, json: Option<&serde_json::Value>) -> String {
    let message = json.and_then(|json| {
        let error = json.get("error");
        error
            .and_then(|error| error.get("message"))
            .or(error.filter(|error| error.is_string()))
            .or_else(|| json.get("message"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    });
    let detail = message.unwrap_or_else(|| body.trim().to_string());
    match detail.char_indices().nth(DETAIL_MAX_CHARS) {
        Some((cut, _)) => format!("{}…", &detail[..cut]),
        None => detail,
    }
}

/// OpenAI's "Please try again in 11.054s" / "28ms"
/// (codex-rs/codex-api/src/sse/responses.rs `try_parse_retry_after`).
fn try_again_in(lower: &str) -> Option<Duration> {
    let rest = lower[lower.find("try again in")? + "try again in".len()..].trim_start();
    let number_len = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    let value: f64 = rest[..number_len].parse().ok()?;
    let unit = rest[number_len..].trim_start();
    if unit.starts_with("ms") {
        Some(Duration::from_secs_f64(value / 1000.0))
    } else if unit.starts_with('s') {
        Some(Duration::from_secs_f64(value))
    } else {
        None
    }
}

fn parse_retry_after(value: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match value.parse::<f64>() {
        Ok(seconds) => delay_after(now, seconds),
        Err(_) => DateTime::parse_from_rfc2822(value)
            .ok()
            .map(|at| at.with_timezone(&Utc)),
    }
}

fn delay_after(now: DateTime<Utc>, seconds: f64) -> Option<DateTime<Utc>> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    let millis = (seconds * 1000.0).ceil().min(i64::MAX as f64) as i64;
    now.checked_add_signed(chrono::Duration::try_milliseconds(millis)?)
}

fn epoch_seconds(value: &str) -> Option<DateTime<Utc>> {
    let seconds = value.parse::<i64>().ok().filter(|seconds| *seconds > 0)?;
    Utc.timestamp_opt(seconds, 0).single()
}

fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
#[path = "provider_limit_tests.rs"]
mod tests;
