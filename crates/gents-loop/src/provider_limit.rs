use std::fmt;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use gents_protocol::provider_limit::{
    ProviderLimitStatement, ProviderLimitVerdict, RecordedProviderLimit,
};

/// The default `max_rate_limit_wait_ms`: the longest provider throttle the
/// owned loop holds before the throttle is the provider's usage limit
/// (Lean: `Runtime.CompletionConfig.defaultMaxWaitMs`).
pub const LONG_RATE_LIMIT_WAIT: Duration = Duration::from_secs(5 * 60);

const MARKER_OPEN: &str = " [provider-limit";
const MARKER_TOKEN: &str = "[provider-limit";
const NEUTRALIZED_TOKEN: &str = "[provider limit";
const USAGE_LIMIT_PREFIX: &str = "provider usage limit reached";
/// Bounds provider-controlled detail included in a usage-limit message.
const DETAIL_MAX_CHARS: usize = 300;

const USAGE_NEEDLES: &[&str] = &[
    "usage limit",
    "usage_limit",
    "usage_not_included",
    "insufficient_quota",
    "exceeded your current quota",
    "quota exceeded",
    "free-usage-exhausted",
    "billing hard limit",
    "credit balance",
    "included free usage",
];

/// The label before the counter a usage refusal states, `USED/LIMIT`.
const TOKEN_COUNTER_LABEL: &str = "tokens (actual/limit):";
/// Window lengths a usage refusal states as `N-UNIT window`, in minutes per unit.
const STATED_WINDOWS: &[(&str, u64)] = &[
    ("-minute window", 1),
    ("-hour window", 60),
    ("-day window", 24 * 60),
];

const THROTTLE_NEEDLES: &[&str] = &["rate_limit", "rate limit", "too many requests"];

const STATUS_PREFIXES: &[&str] = &[
    "invalid status code ",
    "invalid status code: ",
    "invalidstatuscodewithmessage(",
    "status code ",
    "http status ",
    "http ",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderLimitHeaders {
    pub retry_at: Option<DateTime<Utc>>,
    pub reset_at: Option<DateTime<Utc>>,
    pub usage_exhausted: bool,
    /// Another account of the pool can serve the request now.
    pub account_rotated: bool,
}

impl ProviderLimitHeaders {
    /// The limits an annotated failure text carries; none when it has no marker.
    pub fn from_text(text: &str) -> Self {
        Self::parse_marker(split_annotated_body(text).1).unwrap_or_default()
    }

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
            let usage_window = window.is_empty()
                || window
                    .strip_suffix('-')
                    .is_some_and(|window| window.starts_with(|c: char| c.is_ascii_digit()));
            if usage_window && value.eq_ignore_ascii_case("rejected") {
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

    pub fn annotate(&self, body: &str) -> String {
        format!(
            "{}{}",
            body.replace(MARKER_TOKEN, NEUTRALIZED_TOKEN),
            self.marker()
        )
    }

    fn marker(&self) -> String {
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
        if self.account_rotated {
            marker.push_str(" account-rotated");
        }
        marker.push(']');
        marker
    }

    fn parse_marker(marker: &str) -> Option<Self> {
        let fields = marker.strip_prefix(MARKER_OPEN)?.strip_suffix(']')?;
        let mut limits = Self::default();
        for field in fields.split_whitespace() {
            match field.split_once('=') {
                Some(("retry-at", value)) => limits.retry_at = Some(parse_rfc3339(value)?),
                Some(("reset-at", value)) => limits.reset_at = Some(parse_rfc3339(value)?),
                None if field == "usage-exhausted" => limits.usage_exhausted = true,
                None if field == "account-rotated" => limits.account_rotated = true,
                _ => return None,
            }
        }
        Some(limits)
    }
}

/// `text` with its marker rewritten to say another account of the pool can serve now.
pub fn mark_account_rotated(text: &str) -> String {
    ProviderLimitHeaders {
        account_rotated: true,
        ..ProviderLimitHeaders::from_text(text)
    }
    .annotate(split_annotated_body(text).0)
}

pub fn split_provider_limit_marker(text: &str) -> (&str, &str) {
    match split_annotated_body(text) {
        (body, marker) if has_status(&body.to_ascii_lowercase(), None) => (body, marker),
        _ => (text, ""),
    }
}

pub fn split_annotated_body(text: &str) -> (&str, &str) {
    match text.rfind(MARKER_OPEN) {
        Some(start) if ProviderLimitHeaders::parse_marker(&text[start..]).is_some() => {
            text.split_at(start)
        }
        _ => (text, ""),
    }
}

pub fn strip_provider_limit_marker(text: &str) -> &str {
    split_provider_limit_marker(text).0
}

/// A failed call's recorded reason: the usage limit its verdict found,
/// rendered, else its text without the provider-limit marker.
pub fn recorded_failure_reason(text: &str, limit: Option<&ProviderLimit>) -> String {
    match limit {
        Some(ProviderLimit::UsageExhausted(limit)) => limit.to_string(),
        _ => strip_provider_limit_marker(text).to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageLimit {
    pub resets_at: Option<DateTime<Utc>>,
    pub detail: String,
    pub statement: ProviderLimitStatement,
}

impl UsageLimit {
    /// A throttle whose wait the owned loop will not hold, read as the
    /// provider's usage limit resetting when that wait ends.
    pub fn unheld_throttle(text: &str, resets_at: Option<DateTime<Utc>>) -> Self {
        let body = strip_provider_limit_marker(text);
        Self {
            resets_at,
            detail: provider_detail(body, embedded_json(body).as_ref()),
            statement: ProviderLimitStatement::default(),
        }
    }
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
    Throttled { retry_after: Option<Duration> },
    UsageExhausted(UsageLimit),
}

impl ProviderLimit {
    /// The verdict a failed call's row records for this limit.
    pub fn recorded(&self) -> RecordedProviderLimit {
        match self {
            Self::Throttled { .. } => RecordedProviderLimit {
                verdict: ProviderLimitVerdict::Throttled,
                resets_at: None,
                statement: ProviderLimitStatement::default(),
            },
            Self::UsageExhausted(limit) => RecordedProviderLimit {
                verdict: ProviderLimitVerdict::UsageLimited,
                resets_at: limit.resets_at,
                statement: limit.statement,
            },
        }
    }
}

/// A live attempt's limit: a throttle stays a throttle whatever its wait, and
/// the owned loop's retry policy decides how long to hold it
/// (`completion_retry::provider_limit_verdict`).
pub fn classify_attempt_provider_limit(text: &str, now: DateTime<Utc>) -> Option<ProviderLimit> {
    let (body, marker) = split_provider_limit_marker(text);
    let headers = ProviderLimitHeaders::parse_marker(marker).unwrap_or_default();
    let lower = body.to_ascii_lowercase();
    let body_json = embedded_json(body);
    let payment = openrouter_payment_required(&lower, body_json.as_ref());
    let usage_text = USAGE_NEEDLES.iter().any(|needle| lower.contains(needle))
        || payment == Some(PaymentRequired::Exhausted);
    let throttle = THROTTLE_NEEDLES.iter().any(|needle| lower.contains(needle))
        || has_status(&lower, Some("429"))
        || (payment == Some(PaymentRequired::InFlight) && headers.retry_at.is_some());
    if !(usage_text || throttle) {
        return None;
    }

    if usage_text || headers.usage_exhausted {
        // A recorded rendering first: its time is absolute, a relative
        // `resets_in_seconds` in its detail would move with the reader's clock.
        let resets_at = rendered_resets_at(body)
            .or_else(|| {
                body_json
                    .as_ref()
                    .and_then(|json| json_resets_at(json, now))
            })
            .or(headers.reset_at)
            .or(headers.retry_at);
        return Some(ProviderLimit::UsageExhausted(UsageLimit {
            resets_at,
            detail: provider_detail(body, body_json.as_ref()),
            statement: stated_limit(&lower),
        }));
    }

    let retry_at = headers
        .retry_at
        .or_else(|| try_again_in(&lower).and_then(|wait| delay_after(now, wait.as_secs_f64())));
    let retry_after = retry_at.map(|at| (at - now).to_std().unwrap_or(Duration::ZERO));
    Some(ProviderLimit::Throttled { retry_after })
}

/// What a usage refusal's text states about the limit it hit.
///
/// External premise: xAI's free-tier refusal names its window as
/// `rolling N-hour window` and its counter as
/// `tokens (actual/limit): USED/LIMIT`; no other provider states either in
/// its refusal text, and a part a refusal does not state stays unknown.
fn stated_limit(lower: &str) -> ProviderLimitStatement {
    let window_minutes = STATED_WINDOWS.iter().find_map(|(unit, minutes)| {
        let before = &lower[..lower.find(unit)?];
        let digits = &before[before.trim_end_matches(|c: char| c.is_ascii_digit()).len()..];
        digits.parse::<u64>().ok()?.checked_mul(*minutes)
    });
    let counter = lower.find(TOKEN_COUNTER_LABEL).and_then(|at| {
        let rest = lower[at + TOKEN_COUNTER_LABEL.len()..].trim_start();
        let (used, rest) = leading_number(rest)?;
        let (limit, _) = leading_number(rest.strip_prefix('/')?)?;
        Some((used, limit))
    });
    ProviderLimitStatement {
        window_minutes,
        tokens_used: counter.map(|(used, _)| used),
        tokens_limit: counter.map(|(_, limit)| limit),
    }
}

fn leading_number(text: &str) -> Option<(u64, &str)> {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    Some((text[..end].parse().ok()?, &text[end..]))
}

#[derive(PartialEq)]
enum PaymentRequired {
    Exhausted,
    InFlight,
}

fn openrouter_payment_required(
    lower: &str,
    json: Option<&serde_json::Value>,
) -> Option<PaymentRequired> {
    if !has_status(lower, Some("402")) {
        return None;
    }
    match json
        .and_then(|json| json.pointer("/error/metadata/limit_source"))
        .and_then(serde_json::Value::as_str)
    {
        Some("openrouter_key_limit" | "openrouter_credits") => Some(PaymentRequired::Exhausted),
        Some("openrouter_in_flight_budget") => Some(PaymentRequired::InFlight),
        Some(_) => None,
        None => lower
            .contains("insufficient credits")
            .then_some(PaymentRequired::Exhausted),
    }
}

fn has_status(lower: &str, code: Option<&str>) -> bool {
    STATUS_PREFIXES.iter().any(|prefix| {
        lower.match_indices(prefix).any(|(at, _)| {
            let rest = &lower.as_bytes()[at + prefix.len()..];
            rest.len() >= 3
                && rest[..3].iter().all(u8::is_ascii_digit)
                && !rest.get(3).is_some_and(u8::is_ascii_digit)
                && code.is_none_or(|code| &rest[..3] == code.as_bytes())
        })
    })
}

fn embedded_json(text: &str) -> Option<serde_json::Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (start < end)
        .then(|| serde_json::from_str(&text[start..=end]).ok())
        .flatten()
}

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

fn rendered_resets_at(body: &str) -> Option<DateTime<Utc>> {
    let start = body.find("(resets at ")? + "(resets at ".len();
    let end = body[start..].find(')')? + start;
    parse_rfc3339(&body[start..end])
}

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

pub(crate) fn epoch_seconds(value: &str) -> Option<DateTime<Utc>> {
    let seconds = value.parse::<i64>().ok().filter(|seconds| *seconds > 0)?;
    Utc.timestamp_opt(seconds, 0).single()
}

pub(crate) fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
#[path = "provider_limit_tests.rs"]
pub(crate) mod tests;
