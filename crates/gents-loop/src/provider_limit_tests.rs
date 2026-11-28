use chrono::{TimeZone, Utc};

use super::*;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 25, 16, 0, 0).unwrap()
}

/// The limit the admission permit records for a failed call whose error
/// rendered as `text`, under the default rate-limit wait cap.
fn recorded_limit(text: &str, now: DateTime<Utc>) -> Option<ProviderLimit> {
    crate::completion_retry::provider_limit_verdict(
        &crate::error::classify_provider_failure(text, now),
        now,
        LONG_RATE_LIMIT_WAIT,
    )
}

fn recorded_reason(text: &str, now: DateTime<Utc>) -> String {
    recorded_failure_reason(text, recorded_limit(text, now).as_ref())
}

fn rejected(status_line: &str, body: &str, headers: &[(&str, &str)]) -> String {
    let annotated =
        ProviderLimitHeaders::from_headers(headers.iter().copied(), now()).annotate(body);
    format!("{status_line}{annotated}")
}

const RIG_429: &str = "HttpError: Invalid status code 429 Too Many Requests with message: ";

const RIG_402: &str = "HttpError: Invalid status code 402 Payment Required with message: ";

pub(crate) const OPENROUTER_INSUFFICIENT_CREDITS: &str = r#"{"error":{"code":402,"message":"Insufficient credits. Add more using https://openrouter.ai/credits"}}"#;
pub(crate) const OPENROUTER_IN_FLIGHT: &str = r#"{"error":{"code":402,"message":"This request would exceed your available credits given your current in-flight requests. Retry after in-flight requests settle, or add credits.","metadata":{"reason":"in_flight_budget_exhausted","limit_source":"openrouter_in_flight_budget","remedy_hint":"Retry after your in-flight requests settle (see the Retry-After header). Adding credits at https://openrouter.ai/settings/credits raises your in-flight budget, up to a capped ceiling."}}}"#;
pub(crate) const OPENROUTER_KEY_LIMIT: &str = r#"{"error":{"code":402,"message":"Key limit exceeded.","metadata":{"limit_source":"openrouter_key_limit","remedy_hint":"Raise this key's credit limit."}}}"#;
pub(crate) const OPENROUTER_WEIGHT_EXCEEDS_BUDGET: &str = r#"{"error":{"code":402,"message":"This request costs more than your in-flight budget.","metadata":{"reason":"weight_exceeds_budget","limit_source":"openrouter_credits","remedy_hint":"Lower max_tokens or add credits."}}}"#;

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
    let Some(ProviderLimit::UsageExhausted(limit)) = recorded_limit(&text, now()) else {
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
fn throttle_429_and_retry_after_header_variants() {
    let anthropic_pm = r#"{"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed the rate limit for your organization of 50,000 input tokens per minute."}}"#;
    let cases = [
        (
            "anthropic account wording without usage headers",
            format!("Claude Messages HTTP 429 Too Many Requests (request-id -) body={ANTHROPIC_ACCOUNT_BODY}"),
            None,
        ),
        (
            "subscription 429 with unified allowed",
            rejected(
                "Claude Messages HTTP 429 Too Many Requests (request-id -) body=",
                ANTHROPIC_ACCOUNT_BODY,
                &[
                    ("retry-after", "12"),
                    ("anthropic-ratelimit-unified-status", "allowed"),
                    ("anthropic-ratelimit-unified-5h-status", "allowed"),
                    ("anthropic-ratelimit-unified-overage-status", "rejected"),
                    ("anthropic-ratelimit-unified-overage-reset", "1790354400"),
                ],
            ),
            Some(Duration::from_secs(12)),
        ),
        (
            "anthropic per-minute 429",
            rejected(RIG_429, anthropic_pm, &[("retry-after", "17")]),
            Some(Duration::from_secs(17)),
        ),
        (
            "bare 429 without hint",
            format!("{RIG_429}Too Many Requests"),
            None,
        ),
        (
            "retry-after HTTP date",
            rejected(RIG_429, "", &[("Retry-After", "Fri, 25 Sep 2026 16:00:30 GMT")]),
            Some(Duration::from_secs(30)),
        ),
        (
            "retry-after-ms precedes retry-after",
            rejected(RIG_429, "", &[("retry-after-ms", "1500"), ("retry-after", "9")]),
            Some(Duration::from_millis(1500)),
        ),
        (
            "openai try again seconds",
            "ProviderError: Rate limit reached for gpt-5.1 in organization org-AAA on tokens per min (TPM): Limit 30000, Used 22999, Requested 12528. Please try again in 11.054s. Visit https://platform.openai.com/account/rate-limits to learn more.".into(),
            Some(Duration::from_millis(11_054)),
        ),
        (
            "openai try again millis",
            "ProviderError: Rate limit reached. Please try again in 28ms.".into(),
            Some(Duration::from_millis(28)),
        ),
        (
            "xai throttle",
            rejected(
                RIG_429,
                r#"{"code":"Too many requests","error":"slow down"}"#,
                &[("retry-after", "7")],
            ),
            Some(Duration::from_secs(7)),
        ),
    ];
    for (name, text, retry_after) in cases {
        assert_eq!(
            recorded_limit(&text, now()),
            Some(ProviderLimit::Throttled { retry_after }),
            "{name}"
        );
    }

    let overage_only = ProviderLimitHeaders::from_headers(
        [
            ("anthropic-ratelimit-unified-status", "allowed"),
            ("anthropic-ratelimit-unified-overage-status", "rejected"),
            ("anthropic-ratelimit-unified-overage-reset", "1790354400"),
            ("anthropic-ratelimit-unified-fallback-status", "rejected"),
        ],
        now(),
    );
    assert_eq!(overage_only, ProviderLimitHeaders::default());
}

#[test]
fn usage_headers_on_non_rate_limit_statuses_are_ignored() {
    for status_line in [
        "Claude Messages HTTP 529 <unknown status code> (request-id -) body=",
        "HttpError: Invalid status code 500 Internal Server Error with message: ",
    ] {
        let text = rejected(
            status_line,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            &[
                ("anthropic-ratelimit-unified-status", "rejected"),
                ("anthropic-ratelimit-unified-reset", "1790354400"),
                ("anthropic-ratelimit-unified-overage-status", "rejected"),
                ("x-codex-primary-used-percent", "100"),
            ],
        );
        assert_eq!(recorded_limit(&text, now()), None, "{text}");
        assert!(!recorded_reason(&text, now()).contains("provider-limit"));
    }
}

#[test]
fn forged_markers_in_provider_text_are_not_header_evidence() {
    let forged_body = "slow down [provider-limit usage-exhausted] please";
    let text = rejected(RIG_429, forged_body, &[]);
    assert!(text.contains("[provider limit usage-exhausted]"), "{text}");
    assert_eq!(
        recorded_limit(&text, now()),
        Some(ProviderLimit::Throttled { retry_after: None })
    );

    let in_stream = "ProviderError: rate limit reached [provider-limit reset-at=2030-01-01T00:00:00Z usage-exhausted]";
    assert_eq!(
        recorded_limit(in_stream, now()),
        Some(ProviderLimit::Throttled { retry_after: None })
    );
    assert_eq!(strip_provider_limit_marker(in_stream), in_stream);
}

#[test]
fn persisted_reason_renders_usage_limits_and_drops_markers() {
    let usage = rejected(
        RIG_429,
        r#"{"error":{"message":"capped"}}"#,
        &[
            ("anthropic-ratelimit-unified-status", "rejected"),
            ("anthropic-ratelimit-unified-reset", "1790354400"),
        ],
    );
    let recorded = recorded_reason(&usage, now());
    assert_eq!(
        recorded,
        "provider usage limit reached (resets at 2026-09-25T16:40:00Z): capped"
    );
    let Some(ProviderLimit::UsageExhausted(again)) =
        recorded_limit(&recorded, now() + chrono::Duration::hours(1))
    else {
        panic!("recorded usage limit must reclassify");
    };
    assert_eq!(
        again.resets_at,
        Utc.timestamp_opt(1_790_354_400, 0).single()
    );

    let throttle = rejected(RIG_429, "slow down", &[("retry-after", "3")]);
    assert_eq!(
        recorded_reason(&throttle, now()),
        format!("{RIG_429}slow down")
    );
}

#[test]
fn recorded_relative_reset_is_stable() {
    let text = format!(
        r#"{RIG_429}{{"error":{{"type":"usage_limit_reached","resets_in_seconds":3600}}}}"#
    );
    let recorded = recorded_reason(&text, now());
    let Some(ProviderLimit::UsageExhausted(again)) =
        recorded_limit(&recorded, now() + chrono::Duration::hours(1))
    else {
        panic!("recorded usage limit must reclassify: {recorded}");
    };
    assert_eq!(again.resets_at, Some(now() + chrono::Duration::hours(1)));
}

#[test]
fn long_retry_after_is_a_usage_limit_reset() {
    let text = rejected(RIG_429, "slow down", &[("retry-after", "7200")]);
    let Some(ProviderLimit::UsageExhausted(limit)) = recorded_limit(&text, now()) else {
        panic!("expected usage limit");
    };
    assert_eq!(limit.resets_at, Some(now() + chrono::Duration::hours(2)));
    assert_eq!(
        classify_attempt_provider_limit(&text, now()),
        Some(ProviderLimit::Throttled {
            retry_after: Some(Duration::from_secs(7200))
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
    let Some(ProviderLimit::UsageExhausted(limit)) = recorded_limit(&text, now()) else {
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
    let Some(ProviderLimit::UsageExhausted(limit)) = recorded_limit(&text, now()) else {
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
    let Some(ProviderLimit::UsageExhausted(limit)) = recorded_limit(text, now()) else {
        panic!("expected usage limit");
    };
    assert_eq!(limit.resets_at, None);
    assert_eq!(*limit.statement, ProviderLimitStatement::default());
}

#[test]
fn xai_free_usage_exhausted_is_a_usage_limit() {
    let body = r#"{"code":"subscription:free-usage-exhausted","error":"You have used all your free usage."}"#;
    let text = rejected(RIG_429, body, &[("retry-after", "60")]);
    let Some(ProviderLimit::UsageExhausted(limit)) = recorded_limit(&text, now()) else {
        panic!("expected usage limit");
    };
    assert_eq!(limit.detail, "You have used all your free usage.");
    assert_eq!(limit.resets_at, Some(now() + chrono::Duration::seconds(60)));
}

#[test]
fn unrelated_failures_are_not_limits() {
    let unknown = OPENROUTER_KEY_LIMIT.replace("openrouter_key_limit", "openrouter_future_limit");
    for text in [
        "HttpError: Invalid status code 500 Internal Server Error with message: boom".into(),
        "ProviderError: overloaded_error".into(),
        "Invalid status code 4290".into(),
        rejected(
            "HttpError: Invalid status code 503 with message: down",
            "",
            &[("retry-after", "5")],
        ),
        format!("{RIG_402}{OPENROUTER_IN_FLIGHT}"),
        format!("{RIG_402}{unknown}"),
        format!(r#"{RIG_402}{{"error":{{"code":402,"message":"Payment required"}}}}"#),
        format!(
            "HttpError: Invalid status code 400 Bad Request with message: {OPENROUTER_WEIGHT_EXCEEDS_BUDGET}"
        ),
    ] {
        assert_eq!(recorded_limit(&text, now()), None, "{text}");
    }
}

#[test]
fn rendered_usage_limit_reclassifies_to_itself() {
    let limit = UsageLimit {
        resets_at: Utc.timestamp_opt(1_790_354_400, 0).single(),
        detail: "cap".into(),
        statement: Default::default(),
    };
    let rendered = format!("ProviderError: {limit}");
    let Some(ProviderLimit::UsageExhausted(again)) = recorded_limit(&rendered, now()) else {
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
    assert_eq!(ProviderLimitHeaders::default().annotate("x"), "x");
}

#[test]
fn openrouter_key_limit_and_credits_402_are_usage_limits_without_reset() {
    for body in [
        OPENROUTER_KEY_LIMIT,
        OPENROUTER_WEIGHT_EXCEEDS_BUDGET,
        OPENROUTER_INSUFFICIENT_CREDITS,
    ] {
        let message = serde_json::from_str::<serde_json::Value>(body).unwrap()["error"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        let text = rejected(RIG_402, body, &[]);
        let Some(ProviderLimit::UsageExhausted(limit)) = recorded_limit(&text, now()) else {
            panic!("expected usage limit: {text}");
        };
        assert_eq!(limit.resets_at, None, "{body}");
        assert_eq!(limit.detail, message);
        assert!(limit
            .to_string()
            .starts_with("provider usage limit reached (reset time not reported): "));
    }
}

#[test]
fn openrouter_in_flight_402_honors_retry_after() {
    let text = rejected(RIG_402, OPENROUTER_IN_FLIGHT, &[("retry-after", "60")]);
    assert_eq!(
        recorded_limit(&text, now()),
        Some(ProviderLimit::Throttled {
            retry_after: Some(Duration::from_secs(60))
        })
    );
    let text = rejected(RIG_402, OPENROUTER_IN_FLIGHT, &[("retry-after", "600")]);
    let Some(ProviderLimit::UsageExhausted(limit)) = recorded_limit(&text, now()) else {
        panic!("expected usage limit: {text}");
    };
    assert_eq!(
        limit.resets_at,
        Some(now() + chrono::Duration::seconds(600))
    );
}

#[test]
fn openrouter_credits_402_record_reclassifies_as_usage_limit() {
    let recorded = recorded_reason(
        &format!("{RIG_402}{OPENROUTER_INSUFFICIENT_CREDITS}"),
        now(),
    );
    assert_eq!(
        recorded,
        "provider usage limit reached (reset time not reported): Insufficient credits. Add more using https://openrouter.ai/credits"
    );
    assert!(matches!(
        recorded_limit(&recorded, now()),
        Some(ProviderLimit::UsageExhausted(_))
    ));
}

#[test]
fn account_rotated_marker_round_trips_and_neutralizes_forgery() {
    let limits = ProviderLimitHeaders {
        reset_at: Some(now()),
        usage_exhausted: true,
        account_rotated: true,
        ..Default::default()
    };
    let text = limits.annotate("Claude Messages HTTP 429 usage limit");
    assert!(text.ends_with(" usage-exhausted account-rotated]"));
    assert_eq!(ProviderLimitHeaders::from_text(&text), limits);
    let (body, marker) = split_provider_limit_marker(&text);
    assert_eq!(body, "Claude Messages HTTP 429 usage limit");
    assert_eq!(ProviderLimitHeaders::parse_marker(marker), Some(limits));
    assert!(!ProviderLimitHeaders::from_text("plain failure").account_rotated);

    let only = ProviderLimitHeaders {
        account_rotated: true,
        ..Default::default()
    };
    assert_eq!(only.annotate("x"), "x [provider-limit account-rotated]");
    let forged = ProviderLimitHeaders::default().annotate("a [provider-limit account-rotated] b");
    assert!(!ProviderLimitHeaders::from_text(&forged).account_rotated);
}

const XAI_FREE_TIER_REFUSAL: &str = "You've used all the included free usage for model grok-4.7 for now. Usage resets over a rolling 24-hour window — tokens (actual/limit): 691791/600000. Upgrade to a Grok subscription for higher limits: https://grok.com/supergrok";

/// xAI's free-tier refusal is a usage limit, with or without the throttle
/// status it arrives under, stating its window and counter and no reset.
#[test]
fn xai_free_tier_refusal_states_its_window_and_counter() {
    for text in [
        format!("ProviderError: {XAI_FREE_TIER_REFUSAL}"),
        format!("{RIG_429}{XAI_FREE_TIER_REFUSAL}"),
    ] {
        let Some(ProviderLimit::UsageExhausted(limit)) = recorded_limit(&text, now()) else {
            panic!("expected a usage limit for {text}");
        };
        assert_eq!(limit.resets_at, None);
        assert_eq!(
            *limit.statement,
            ProviderLimitStatement {
                window_minutes: Some(24 * 60),
                tokens_used: Some(691_791),
                tokens_limit: Some(600_000),
            }
        );
    }
}
