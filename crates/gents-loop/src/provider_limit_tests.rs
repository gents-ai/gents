use chrono::{TimeZone, Utc};

use super::*;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 25, 16, 0, 0).unwrap()
}

fn rejected(status_line: &str, body: &str, headers: &[(&str, &str)]) -> String {
    let marker = ProviderLimitHeaders::from_headers(headers.iter().copied(), now()).marker();
    format!("{status_line}{body}{marker}")
}

const RIG_429: &str = "HttpError: Invalid status code 429 Too Many Requests with message: ";

/// #1422 live repro: Claude subscription seat capped until 11:40 local.
const ANTHROPIC_ACCOUNT_BODY: &str = r#"{"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed your account's rate limit. Please try again later."},"request_id":"req_011CeoNrgs1EJvDwA2YxXE7R"}"#;

#[test]
fn anthropic_subscription_cap_reports_unified_reset() {
    let text = rejected(
        "Claude Messages HTTP 429 Too Many Requests (request-id -) body=",
        ANTHROPIC_ACCOUNT_BODY,
        &[
            ("retry-after", "6000"),
            ("anthropic-ratelimit-unified-status", "rejected"),
            ("anthropic-ratelimit-unified-reset", "1790354400"),
            ("anthropic-ratelimit-unified-5h-status", "rejected"),
            ("anthropic-ratelimit-unified-5h-reset", "1790354400"),
            ("anthropic-ratelimit-unified-7d-status", "allowed"),
            ("anthropic-ratelimit-unified-7d-reset", "1790900000"),
        ],
    );
    let Some(ProviderLimit::UsageExhausted(limit)) = classify_provider_limit(&text, now()) else {
        panic!("expected usage limit: {text}");
    };
    assert_eq!(
        limit.resets_at,
        Utc.timestamp_opt(1_790_354_400, 0).single()
    );
    assert_eq!(
        limit.detail,
        "This request would exceed your account's rate limit. Please try again later."
    );
    assert!(limit.to_string().starts_with(
        "provider usage limit reached (resets at 2026-09-25T16:40:00Z): This request"
    ));
}

#[test]
fn anthropic_account_wording_without_headers_is_a_usage_limit() {
    let text = format!(
        "Claude Messages HTTP 429 Too Many Requests (request-id -) body={ANTHROPIC_ACCOUNT_BODY}"
    );
    let Some(ProviderLimit::UsageExhausted(limit)) = classify_provider_limit(&text, now()) else {
        panic!("expected usage limit");
    };
    assert_eq!(limit.resets_at, None);
    assert!(limit.to_string().contains("reset time not reported"));
}

#[test]
fn anthropic_per_minute_429_honors_retry_after() {
    let body = r#"{"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed the rate limit for your organization of 50,000 input tokens per minute."}}"#;
    let text = rejected(RIG_429, body, &[("retry-after", "17")]);
    assert_eq!(
        classify_provider_limit(&text, now()),
        Some(ProviderLimit::Throttled {
            retry_after: Some(Duration::from_secs(17))
        })
    );
}

#[test]
fn bare_429_without_hint_is_an_unhinted_throttle() {
    let text = format!("{RIG_429}Too Many Requests");
    assert_eq!(
        classify_provider_limit(&text, now()),
        Some(ProviderLimit::Throttled { retry_after: None })
    );
}

#[test]
fn long_retry_after_is_a_usage_limit_reset() {
    let text = rejected(RIG_429, "slow down", &[("retry-after", "7200")]);
    let Some(ProviderLimit::UsageExhausted(limit)) = classify_provider_limit(&text, now()) else {
        panic!("expected usage limit");
    };
    assert_eq!(limit.resets_at, Some(now() + chrono::Duration::hours(2)));
}

#[test]
fn retry_after_http_date_and_milliseconds_are_honored() {
    let date = rejected(
        RIG_429,
        "",
        &[("Retry-After", "Fri, 25 Sep 2026 16:00:30 GMT")],
    );
    assert_eq!(
        classify_provider_limit(&date, now()),
        Some(ProviderLimit::Throttled {
            retry_after: Some(Duration::from_secs(30))
        })
    );
    let millis = rejected(
        RIG_429,
        "",
        &[("retry-after-ms", "1500"), ("retry-after", "9")],
    );
    assert_eq!(
        classify_provider_limit(&millis, now()),
        Some(ProviderLimit::Throttled {
            retry_after: Some(Duration::from_millis(1500))
        })
    );
}

#[test]
fn codex_usage_limit_reached_body_carries_reset() {
    let body = r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"plus","resets_at":1790354400}}"#;
    let text = rejected(
        RIG_429,
        body,
        &[
            ("x-codex-primary-used-percent", "100.0"),
            ("x-codex-primary-reset-at", "1790350000"),
        ],
    );
    let Some(ProviderLimit::UsageExhausted(limit)) = classify_provider_limit(&text, now()) else {
        panic!("expected usage limit");
    };
    assert_eq!(
        limit.resets_at,
        Utc.timestamp_opt(1_790_354_400, 0).single()
    );
    assert_eq!(limit.detail, "The usage limit has been reached");
}

#[test]
fn codex_exhausted_window_header_alone_is_a_usage_limit() {
    let text = rejected(
        RIG_429,
        "{}",
        &[
            ("x-codex-primary-used-percent", "42"),
            ("x-codex-secondary-used-percent", "100"),
            ("x-codex-secondary-reset-at", "1790354400"),
        ],
    );
    let Some(ProviderLimit::UsageExhausted(limit)) = classify_provider_limit(&text, now()) else {
        panic!("expected usage limit");
    };
    assert_eq!(
        limit.resets_at,
        Utc.timestamp_opt(1_790_354_400, 0).single()
    );
}

#[test]
fn openai_insufficient_quota_stream_failure_is_a_usage_limit() {
    let text = "ProviderError: You exceeded your current quota, please check your plan and billing details. (insufficient_quota)";
    assert!(matches!(
        classify_provider_limit(text, now()),
        Some(ProviderLimit::UsageExhausted(UsageLimit {
            resets_at: None,
            ..
        }))
    ));
}

#[test]
fn openai_rate_limit_exceeded_try_again_hint_is_a_throttle() {
    let text = "ProviderError: Rate limit reached for gpt-5.1 in organization org-AAA on tokens per min (TPM): Limit 30000, Used 22999, Requested 12528. Please try again in 11.054s. Visit https://platform.openai.com/account/rate-limits to learn more.";
    assert_eq!(
        classify_provider_limit(text, now()),
        Some(ProviderLimit::Throttled {
            retry_after: Some(Duration::from_millis(11_054))
        })
    );
    let millis = "ProviderError: Rate limit reached. Please try again in 28ms.";
    assert_eq!(
        classify_provider_limit(millis, now()),
        Some(ProviderLimit::Throttled {
            retry_after: Some(Duration::from_millis(28))
        })
    );
}

#[test]
fn xai_free_usage_exhausted_is_a_usage_limit() {
    let body = r#"{"code":"subscription:free-usage-exhausted","error":"You have used all your free usage."}"#;
    let text = rejected(RIG_429, body, &[("retry-after", "60")]);
    let Some(ProviderLimit::UsageExhausted(limit)) = classify_provider_limit(&text, now()) else {
        panic!("expected usage limit");
    };
    assert_eq!(limit.detail, "You have used all your free usage.");
    assert_eq!(limit.resets_at, Some(now() + chrono::Duration::seconds(60)));
}

#[test]
fn xai_throttle_honors_retry_after() {
    let text = rejected(
        RIG_429,
        r#"{"code":"Too many requests","error":"slow down"}"#,
        &[("retry-after", "7")],
    );
    assert_eq!(
        classify_provider_limit(&text, now()),
        Some(ProviderLimit::Throttled {
            retry_after: Some(Duration::from_secs(7))
        })
    );
}

#[test]
fn unrelated_failures_are_not_limits() {
    for text in [
        "HttpError: Invalid status code 500 Internal Server Error with message: boom",
        "ProviderError: overloaded_error",
        "Invalid status code 4290",
    ] {
        assert_eq!(classify_provider_limit(text, now()), None, "{text}");
    }
    let marker_only = rejected(
        "HttpError: Invalid status code 503 with message: down",
        "",
        &[("retry-after", "5")],
    );
    assert_eq!(classify_provider_limit(&marker_only, now()), None);
}

#[test]
fn rendered_usage_limit_reclassifies_to_itself() {
    let limit = UsageLimit {
        resets_at: Utc.timestamp_opt(1_790_354_400, 0).single(),
        detail: "cap".into(),
    };
    let rendered = format!("ProviderError: {limit}");
    let Some(ProviderLimit::UsageExhausted(again)) = classify_provider_limit(&rendered, now())
    else {
        panic!("expected usage limit");
    };
    assert_eq!(again.resets_at, limit.resets_at);
}

#[test]
fn marker_survives_body_bounding() {
    let text = rejected(RIG_429, "x", &[("retry-after", "3")]);
    let (body, marker) = split_provider_limit_marker(&text);
    assert!(body.ends_with('x'));
    assert_eq!(marker, " [provider-limit retry-at=2026-09-25T16:00:03Z]");
    assert_eq!(ProviderLimitHeaders::default().marker(), "");
}
