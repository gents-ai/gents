use std::fmt;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};

/// Treat long provider waits as usage exhaustion so transport retries do not
/// hold the request until a distant reset; the reset time remains observable.
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
    // A free tier's own wording. xAI/Grok says "used all the included free
    // usage", which matched none of the needles above, so that refusal used to
    // classify as neither exhausted nor throttled and no read path could see it.
    //
    // Narrow on purpose: a bare "free usage" matched any free-tier mention,
    // including "free usage ... 20 requests/minute", which is a throttle and was
    // becoming a permanent exhaustion. These three are the shapes that state
    // exhaustion, and each is unreachable from the others.
    "included free usage",
    "used all the included",
    "exhausted your free",
    "free usage exhausted",
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

pub fn persisted_failure_reason(text: &str, now: DateTime<Utc>) -> String {
    match classify_provider_limit(text, now) {
        Some(ProviderLimit::UsageExhausted(limit)) => limit.to_string(),
        _ => strip_provider_limit_marker(text).to_string(),
    }
}

/// The windows providers actually state, as a closed set.
///
/// Closed on purpose: the window reaches `for_model`, so a free-text window
/// would put provider text in front of a model, and an unbounded string is
/// something a hostile body can forge out of. Parsed once, at classification
/// time, from the body; the enum is what is carried everywhere after that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LimitWindow {
    /// xAI's own words: "rolling 24-hour window". Only the word "rolling" earns
    /// this label: it states something a plain 24-hour period does not, namely
    /// that the window slides rather than resetting on a boundary.
    Rolling24h,
    /// "24-hour window"/"24 hour window" without "rolling". Its own label, so a
    /// reader is not told the window rolls when the provider never said so.
    Window24h,
    Daily,
    Weekly,
    Monthly,
}

impl LimitWindow {
    /// The label every surface shows. This is the only spelling that exists, so
    /// nothing outside this enum can appear as a window.
    pub fn label(self) -> &'static str {
        match self {
            LimitWindow::Rolling24h => "rolling 24-hour",
            LimitWindow::Window24h => "24-hour",
            LimitWindow::Daily => "daily",
            LimitWindow::Weekly => "weekly",
            LimitWindow::Monthly => "monthly",
        }
    }

    /// The marker's token, which is also what a reader parses back.
    pub fn token(self) -> &'static str {
        match self {
            LimitWindow::Rolling24h => "rolling_24h",
            LimitWindow::Window24h => "24h",
            LimitWindow::Daily => "daily",
            LimitWindow::Weekly => "weekly",
            LimitWindow::Monthly => "monthly",
        }
    }

    /// The inverse of [`Self::token`]. Anything else — a forged or newer token —
    /// is no window at all rather than a guess.
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "rolling_24h" => Some(LimitWindow::Rolling24h),
            "24h" => Some(LimitWindow::Window24h),
            "daily" => Some(LimitWindow::Daily),
            "weekly" => Some(LimitWindow::Weekly),
            "monthly" => Some(LimitWindow::Monthly),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageLimit {
    pub resets_at: Option<DateTime<Utc>>,
    pub detail: String,
    /// The window the provider named, when it named one. `None` means the
    /// provider did not state it, not that it is 24 hours.
    pub window: Option<LimitWindow>,
    /// The counter the provider reported, e.g. 691791 of 600000.
    pub used: Option<u64>,
    /// The cap the provider named. `None` means unstated — never inferred from
    /// `used`, which would invent a limit.
    pub cap: Option<u64>,
}

/// What the provider stated, as a marker beside the human sentence.
///
/// The parsed fields are what every read path needs (window, counter, cap), and
/// the persisted `failure_reason` is the only place they can survive: it holds
/// this `Display` text whole, while `detail` inside it is bounded to
/// `DETAIL_MAX_CHARS`. The marker is therefore written from the parsed fields,
/// not re-read out of the prose, and it is written **first** so a long detail
/// cannot truncate it away. A row persisted before this change has no marker and
/// still parses from the prose it kept.
const LIMIT_OPEN: &str = "[usage-limit";
const LIMIT_CLOSE: char = ']';

// The marker as it sits in a persisted reason: `[usage-limit used=691791
// cap=600000 window=rolling_24h]`. Every token is either a decimal `u64` or a
// member of `LimitWindow`'s closed set, so nothing a provider wrote can become a
// field: there is no free text in it at all.
impl fmt::Display for UsageLimit {
    /// `provider usage limit reached [usage-limit …] (resets at …): detail`.
    ///
    /// The marker sits **immediately** after the prefix and before everything
    /// else, so a reader can find it by position rather than by searching: a
    /// provider body that merely contains a marker-shaped string cannot be
    /// mistaken for one this node wrote, because it is not at that position.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{USAGE_LIMIT_PREFIX}")?;
        let marker = self.statement_marker();
        if !marker.is_empty() {
            write!(f, " {marker}")?;
        }
        match self.resets_at {
            Some(at) => write!(f, " (resets at {})", rfc3339(at))?,
            None => write!(f, " (reset time not reported)")?,
        }
        if !self.detail.is_empty() {
            write!(f, ": {}", self.detail)?;
        }
        Ok(())
    }
}

impl UsageLimit {
    /// `[used=691791 cap=600000 window=rolling_24h]`, or empty when the
    /// provider stated nothing beyond the refusal.
    fn statement_marker(&self) -> String {
        if self.used.is_none() && self.cap.is_none() && self.window.is_none() {
            return String::new();
        }
        let mut marker = String::from(LIMIT_OPEN);
        if let Some(used) = self.used {
            marker.push_str(&format!(" used={used}"));
        }
        if let Some(cap) = self.cap {
            marker.push_str(&format!(" cap={cap}"));
        }
        if let Some(window) = self.window {
            // A closed-set token, so a window can never carry a tab, a newline,
            // a `]` or another field.
            marker.push_str(&format!(" window={}", window.token()));
        }
        marker.push(LIMIT_CLOSE);
        marker
    }
}

/// The statement a persisted `failure_reason` carries, if it carries one.
///
/// Found by **position**, not by searching: the text must *start* with this
/// node's prefix and the marker must be the very next token. A body that merely
/// contains `[usage-limit …]` — because a provider echoed it, or because the
/// provider wrote it — is not a marker this node wrote, so it cannot forge one.
///
/// Every field is also constrained: `used`/`cap` are decimal and `cap > 0`, and
/// `window` is a closed-set token. A field that does not parse is not a field.
fn stored_statement(text: &str) -> UsageStatement {
    let Some(after_prefix) = text.strip_prefix(USAGE_LIMIT_PREFIX) else {
        return UsageStatement::default();
    };
    let Some(rest) = after_prefix.trim_start().strip_prefix(LIMIT_OPEN) else {
        return UsageStatement::default();
    };
    let Some(end) = rest.find(LIMIT_CLOSE) else {
        return UsageStatement::default();
    };
    // A marker this node wrote has no whitespace inside a field and no field
    // outside the three it knows; a body that merely starts with the prefix and
    // then contains a bracket is not one.
    let inside = &rest[..end];
    if inside.chars().any(|c| c.is_control()) {
        return UsageStatement::default();
    }
    let mut statement = UsageStatement::default();
    let mut saw_field = false;
    for field in inside.split_whitespace() {
        match field.split_once('=') {
            Some(("used", value)) => {
                statement.used = value.parse::<u64>().ok();
                saw_field = true;
            }
            Some(("cap", value)) => {
                statement.cap = value.parse::<u64>().ok().filter(|cap| *cap > 0);
                saw_field = true;
            }
            Some(("window", value)) => {
                statement.window = LimitWindow::from_token(value);
                saw_field = true;
            }
            // An unknown token means this is not a marker this node wrote.
            _ => return UsageStatement::default(),
        }
    }
    if !saw_field {
        return UsageStatement::default();
    }
    statement
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderLimit {
    Throttled { retry_after: Option<Duration> },
    UsageExhausted(UsageLimit),
}

pub fn classify_provider_limit(text: &str, now: DateTime<Utc>) -> Option<ProviderLimit> {
    let (body, marker) = split_provider_limit_marker(text);
    let headers = ProviderLimitHeaders::parse_marker(marker).unwrap_or_default();
    let lower = body.to_ascii_lowercase();
    let body_json = embedded_json(body);
    let payment = openrouter_payment_required(&lower, body_json.as_ref());
    // A sub-day rate is a throttle however much quota wording rides along with
    // it: "free usage ... 20 requests/minute" is a wait, not a spent account, and
    // treating it as exhaustion blocks a goal until a reset it never stated.
    // This is decided first, so a per-minute rate outranks the usage needles.
    let sub_day_rate = sub_day_rate_window(&lower);
    let usage_text = !sub_day_rate
        && (USAGE_NEEDLES.iter().any(|needle| lower.contains(needle))
            || payment == Some(PaymentRequired::Exhausted));
    let throttle = sub_day_rate
        || THROTTLE_NEEDLES.iter().any(|needle| lower.contains(needle))
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
        let statement = statement_of(body, &lower, body_json.as_ref());
        return Some(ProviderLimit::UsageExhausted(UsageLimit {
            resets_at,
            detail: provider_detail(body, body_json.as_ref()),
            window: statement.window,
            used: statement.used,
            cap: statement.cap,
        }));
    }

    let retry_at = headers
        .retry_at
        .or_else(|| try_again_in(&lower).and_then(|wait| delay_after(now, wait.as_secs_f64())));
    let retry_after = retry_at.map(|at| (at - now).to_std().unwrap_or(Duration::ZERO));
    if retry_after.is_some_and(|wait| wait > LONG_RATE_LIMIT_WAIT) {
        let statement = statement_of(body, &lower, body_json.as_ref());
        return Some(ProviderLimit::UsageExhausted(UsageLimit {
            resets_at: retry_at,
            detail: provider_detail(body, body_json.as_ref()),
            window: statement.window,
            used: statement.used,
            cap: statement.cap,
        }));
    }
    Some(ProviderLimit::Throttled { retry_after })
}

/// What a refusal states beyond "no": the window it names and the counter it
/// reports. Every field is optional, because a refusal establishes only what it
/// says — plain "free usage exhausted" gives no cap and no reset, and inventing
/// either would be worse than saying unknown.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct UsageStatement {
    window: Option<LimitWindow>,
    used: Option<u64>,
    cap: Option<u64>,
}

/// The statement a body states: the marker this node persisted when it first
/// classified the refusal, or — for a row written before the marker existed —
/// a parse of the prose it kept.
///
/// The marker wins field by field, so a provider's exact counter and cap survive
/// both the bounded `detail` and any later change to the needle set. A marker
/// that states nothing falls through to the prose.
fn statement_of(body: &str, lower: &str, body_json: Option<&serde_json::Value>) -> UsageStatement {
    let stored = stored_statement(body);
    let mut statement = UsageStatement::parse(lower, body_json);
    if stored.window.is_some() {
        statement.window = stored.window;
    }
    if stored.used.is_some() {
        statement.used = stored.used;
    }
    if stored.cap.is_some() {
        statement.cap = stored.cap;
    }
    statement
}

impl UsageStatement {
    /// Parse the statement out of a lowercased body and, when there is one, a
    /// structured JSON body. The JSON wins where it speaks, since it is the
    /// provider's own field rather than prose.
    fn parse(lower: &str, body_json: Option<&serde_json::Value>) -> Self {
        let mut statement = Self::default();
        if let Some(window) = named_window(lower) {
            statement.window = Some(window);
        }
        if let Some(json) = body_json {
            statement.window = statement.window.or_else(|| {
                json_str(json, &["window", "period", "quota_window"])
                    .as_deref()
                    .and_then(json_window)
            });
            statement.used = json_u64(json, &["used", "usage", "tokens_used", "consumed"]);
            // A JSON `limit: 0` is not a cap either; Lean `establishes` requires
            // cap > 0, so zero is unstated rather than a spent allowance.
            statement.cap =
                json_u64(json, &["cap", "limit", "tokens_limit", "quota"]).filter(|cap| *cap > 0);
        }
        // `tokens 691791/600000` — the pair is only read when both sides parse,
        // so a malformed pair leaves used and cap unstated together rather than
        // pairing one real number with a guess.
        if statement.used.is_none() || statement.cap.is_none() {
            if let Some((used, cap)) = token_pair(lower) {
                statement.used.get_or_insert(used);
                statement.cap.get_or_insert(cap);
            }
        }
        statement
    }
}

/// A window named in prose.
///
/// The hour-shaped spellings name the window themselves, so finding them is
/// enough. A bare `daily`/`weekly`/`monthly` does not: an exhaustion body can
/// say "your daily allowance" or "daily credits" without the provider meaning
/// that period as the enforced limit, and reporting it as the provider's window
/// would assert something it never said. Those count only beside the word
/// `window`, or in a "limit"/"quota" phrase.
fn named_window(lower: &str) -> Option<LimitWindow> {
    for needle in ["rolling 24-hour", "rolling 24 hour"] {
        if lower.contains(needle) {
            return Some(LimitWindow::Rolling24h);
        }
    }
    for needle in ["24-hour window", "24 hour window"] {
        if lower.contains(needle) {
            // No "rolling" in the body, so it is a 24-hour window and not a
            // rolling one: only the provider saying "rolling" earns that label.
            return Some(LimitWindow::Window24h);
        }
    }
    for (period, window) in [
        ("daily", LimitWindow::Daily),
        ("weekly", LimitWindow::Weekly),
        ("monthly", LimitWindow::Monthly),
    ] {
        if period_window_stated(lower, period) {
            return Some(window);
        }
    }
    None
}

/// A JSON window field, mapped into the closed set. Anything else is no window,
/// so a provider's free text cannot become the window a model reads.
fn json_window(text: &str) -> Option<LimitWindow> {
    let lower = text.to_ascii_lowercase();
    let squashed: String = lower
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let squashed = squashed.trim_matches('-');
    let collapsed = squashed
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    match collapsed.as_str() {
        // Only an explicit "rolling" is a rolling window; a bare 24-hour period
        // is its own label.
        "rolling-24-hour" | "rolling-24h" | "rolling-24-hours" => Some(LimitWindow::Rolling24h),
        "24-hour" | "24h" | "24-hours" => Some(LimitWindow::Window24h),
        "daily" | "day" => Some(LimitWindow::Daily),
        "weekly" | "week" => Some(LimitWindow::Weekly),
        "monthly" | "month" => Some(LimitWindow::Monthly),
        // Anything else is no window: provider free text never becomes one.
        _ => None,
    }
}

/// Whether `period` appears in the body as the stated limit window: "daily
/// window", "window: daily", "daily limit", "daily quota". A bare mention is
/// not enough.
fn period_window_stated(lower: &str, period: &str) -> bool {
    let mut from = 0;
    while let Some(found) = lower[from..].find(period) {
        let at = from + found;
        let before = &lower[..at];
        let after = &lower[at + period.len()..];
        // `split_whitespace` already skips the leading space.
        let next_word = after.split_whitespace().next().unwrap_or("");
        let prev_word = before.split_whitespace().next_back().unwrap_or("");
        let names_window = matches!(next_word, "window" | "limit" | "quota" | "period")
            || matches!(prev_word, "window" | "window:" | "limit" | "quota");
        if names_window {
            return true;
        }
        from = at + period.len();
    }
    false
}

/// Whether the body names a rate faster than a day — per minute, second or hour.
/// Such a rate is a throttle's window, and outranks quota wording beside it.
fn sub_day_rate_window(lower: &str) -> bool {
    // A rate word standing on its own. Matching bare substrings made `/security`
    // and `/minimum` read as "per second"/"per minute" and turned a genuine
    // exhaustion into a throttle, so each hit is checked for a clean boundary
    // before it counts.
    const RATE_WORDS: &[&str] = &[
        "min", "minute", "minutes", "sec", "second", "seconds", "hr", "hour", "hours",
    ];
    // Phrases that already contain their own boundaries.
    const RATE_PHRASES: &[&str] = &[
        "per minute",
        "per-minute",
        "per second",
        "per-second",
        "per hour",
        "per-hour",
    ];
    if RATE_PHRASES.iter().any(|needle| lower.contains(needle)) {
        return true;
    }
    // `rpm`/`tpm` as a standalone word, not the tail of another word.
    for word in ["rpm", "tpm"] {
        if token_present(lower, word) {
            return true;
        }
    }
    // `/min`, `/sec`, `/hr` and friends: the character after the unit must not
    // continue the word, which is what keeps `/security` out.
    for marker in ['/'] {
        let mut from = 0;
        while let Some(found) = lower[from..].find(marker) {
            let after = &lower[from + found + marker.len_utf8()..];
            for unit in RATE_WORDS {
                if let Some(tail) = after.strip_prefix(unit) {
                    // The unit must end where the word ends: `/security` fails on
                    // 'u', and `/hours-left` fails on '-' — a path segment, not a
                    // rate. Only a separator or the end of the text counts.
                    if tail
                        .chars()
                        .next()
                        .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                    {
                        return true;
                    }
                }
            }
            from = from + found + marker.len_utf8();
        }
    }
    false
}

/// Whether `word` appears as its own token: bounded by non-alphanumerics.
fn token_present(lower: &str, word: &str) -> bool {
    let mut from = 0;
    while let Some(found) = lower[from..].find(word) {
        let at = from + found;
        let before_ok = lower[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        let after_ok = lower[at + word.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        from = at + word.len();
    }
    false
}

/// `tokens 691791/600000`, anchored on the word `tokens`.
///
/// Anchoring matters: taking the first `digits/digits` anywhere read dates
/// (`4/5/2026`) and ratios as a counter and a cap. Both sides must parse and the
/// cap must be non-zero — Lean `Refusal.establishes` requires `cap > 0`, so a
/// `tokens 0/0` is a pair that establishes nothing rather than a cap of zero.
fn token_pair(lower: &str) -> Option<(u64, u64)> {
    let mut from = 0;
    while let Some(found) = lower[from..].find("tokens") {
        let after = &lower[from + found + "tokens".len()..];
        if let Some(pair) = pair_after_anchor(after) {
            return Some(pair);
        }
        from = from + found + "tokens".len();
    }
    None
}

/// The first `used/cap` at the start of `text`, allowing the lead-ins providers
/// actually put between the word `tokens` and the digits:
///
/// * `tokens 691791/600000`
/// * `tokens: 691791/600000`
/// * `tokens (actual/limit): 691791/600000` — xAI's own shape
///
/// The parenthesised label is skipped as a whole, which matters because it
/// contains a `/` of its own: reading that as the separator gives no pair at all.
fn pair_after_anchor(text: &str) -> Option<(u64, u64)> {
    let mut rest = text.trim_start();
    // A parenthesised label, then whatever punctuation follows it.
    if let Some(after_open) = rest.strip_prefix('(') {
        let close = after_open.find(')')?;
        rest = after_open[close + 1..].trim_start();
    }
    // One optional lead-in word, then optional punctuation and space.
    for lead in [":", "used", "remaining", "of", "=", "-"] {
        if let Some(tail) = rest.strip_prefix(lead) {
            rest = tail.trim_start();
            break;
        }
    }
    let rest = rest.trim_start_matches([':', ' ', '=']);
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;
    if !first.is_ascii_digit() {
        return None;
    }
    let left_len = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '_')
        .map(char::len_utf8)
        .sum::<usize>();
    let left = rest[..left_len].replace([',', '_'], "");
    let after_left = &rest[left_len..];
    let after_left = after_left.trim_start();
    let after_left = after_left.strip_prefix('/')?.trim_start();
    let right_len = after_left
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '_')
        .map(char::len_utf8)
        .sum::<usize>();
    if right_len == 0 {
        return None;
    }
    let right = after_left[..right_len].replace([',', '_'], "");
    let used = left.parse::<u64>().ok()?;
    let cap = right.parse::<u64>().ok()?;
    if cap == 0 {
        return None;
    }
    Some((used, cap))
}

/// The first of `keys` present as a string anywhere the JSON reaches.
fn json_str(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    find_json(value, keys).and_then(|found| found.as_str().map(str::to_owned))
}

/// The first of `keys` present as an unsigned integer.
fn json_u64(value: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    find_json(value, keys).and_then(|found| found.as_u64())
}

fn find_json<'a>(value: &'a serde_json::Value, keys: &[&str]) -> Option<&'a serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => {
            for key in keys {
                if let Some(found) = map.get(*key) {
                    return Some(found);
                }
            }
            map.values().find_map(|nested| find_json(nested, keys))
        }
        serde_json::Value::Array(items) => items.iter().find_map(|item| find_json(item, keys)),
        _ => None,
    }
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
