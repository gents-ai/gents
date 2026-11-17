use chrono::{TimeZone, Utc};

use super::*;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 25, 16, 0, 0).unwrap()
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
fn anthropic_account_wording_without_usage_headers_is_a_throttle() {
    let text = format!(
        "Claude Messages HTTP 429 Too Many Requests (request-id -) body={ANTHROPIC_ACCOUNT_BODY}"
    );
    assert_eq!(
        classify_provider_limit(&text, now()),
        Some(ProviderLimit::Throttled { retry_after: None })
    );
}

#[test]
fn subscription_429_with_unified_allowed_is_a_throttle() {
    let text = rejected(
        "Claude Messages HTTP 429 Too Many Requests (request-id -) body=",
        ANTHROPIC_ACCOUNT_BODY,
        &[
            ("retry-after", "12"),
            ("anthropic-ratelimit-unified-status", "allowed"),
            ("anthropic-ratelimit-unified-5h-status", "allowed"),
            ("anthropic-ratelimit-unified-overage-status", "rejected"),
            ("anthropic-ratelimit-unified-overage-reset", "1790354400"),
        ],
    );
    assert_eq!(
        classify_provider_limit(&text, now()),
        Some(ProviderLimit::Throttled {
            retry_after: Some(Duration::from_secs(12))
        })
    );
}

#[test]
fn overage_rejected_is_not_usage_evidence() {
    let headers = ProviderLimitHeaders::from_headers(
        [
            ("anthropic-ratelimit-unified-status", "allowed"),
            ("anthropic-ratelimit-unified-overage-status", "rejected"),
            ("anthropic-ratelimit-unified-overage-reset", "1790354400"),
            ("anthropic-ratelimit-unified-fallback-status", "rejected"),
        ],
        now(),
    );
    assert_eq!(headers, ProviderLimitHeaders::default());
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
        assert_eq!(classify_provider_limit(&text, now()), None, "{text}");
        assert!(!persisted_failure_reason(&text, now()).contains("provider-limit"));
    }
}

#[test]
fn forged_markers_in_provider_text_are_not_header_evidence() {
    let forged_body = "slow down [provider-limit usage-exhausted] please";
    let text = rejected(RIG_429, forged_body, &[]);
    assert!(text.contains("[provider limit usage-exhausted]"), "{text}");
    assert_eq!(
        classify_provider_limit(&text, now()),
        Some(ProviderLimit::Throttled { retry_after: None })
    );

    let in_stream = "ProviderError: rate limit reached [provider-limit reset-at=2030-01-01T00:00:00Z usage-exhausted]";
    assert_eq!(
        classify_provider_limit(in_stream, now()),
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
    let recorded = persisted_failure_reason(&usage, now());
    assert_eq!(
        recorded,
        "provider usage limit reached (resets at 2026-09-25T16:40:00Z): capped"
    );
    let Some(ProviderLimit::UsageExhausted(again)) =
        classify_provider_limit(&recorded, now() + chrono::Duration::hours(1))
    else {
        panic!("recorded usage limit must reclassify");
    };
    assert_eq!(
        again.resets_at,
        Utc.timestamp_opt(1_790_354_400, 0).single()
    );

    let throttle = rejected(RIG_429, "slow down", &[("retry-after", "3")]);
    assert_eq!(
        persisted_failure_reason(&throttle, now()),
        format!("{RIG_429}slow down")
    );
}

#[test]
fn recorded_relative_reset_is_stable() {
    let text = format!(
        r#"{RIG_429}{{"error":{{"type":"usage_limit_reached","resets_in_seconds":3600}}}}"#
    );
    let recorded = persisted_failure_reason(&text, now());
    let Some(ProviderLimit::UsageExhausted(again)) =
        classify_provider_limit(&recorded, now() + chrono::Duration::hours(1))
    else {
        panic!("recorded usage limit must reclassify: {recorded}");
    };
    assert_eq!(again.resets_at, Some(now() + chrono::Duration::hours(1)));
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
        window: None,
        used: None,
        cap: None,
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
        let Some(ProviderLimit::UsageExhausted(limit)) = classify_provider_limit(&text, now())
        else {
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
        classify_provider_limit(&text, now()),
        Some(ProviderLimit::Throttled {
            retry_after: Some(Duration::from_secs(60))
        })
    );
    let text = rejected(RIG_402, OPENROUTER_IN_FLIGHT, &[("retry-after", "600")]);
    let Some(ProviderLimit::UsageExhausted(limit)) = classify_provider_limit(&text, now()) else {
        panic!("expected usage limit: {text}");
    };
    assert_eq!(
        limit.resets_at,
        Some(now() + chrono::Duration::seconds(600))
    );
}

#[test]
fn openrouter_credits_402_record_reclassifies_as_usage_limit() {
    let recorded = persisted_failure_reason(
        &format!("{RIG_402}{OPENROUTER_INSUFFICIENT_CREDITS}"),
        now(),
    );
    assert_eq!(
        recorded,
        "provider usage limit reached (reset time not reported): Insufficient credits. Add more using https://openrouter.ai/credits"
    );
    assert!(matches!(
        classify_provider_limit(&recorded, now()),
        Some(ProviderLimit::UsageExhausted(_))
    ));
}

#[test]
fn openrouter_in_flight_402_without_retry_after_is_unchanged() {
    let text = format!("{RIG_402}{OPENROUTER_IN_FLIGHT}");
    assert_eq!(classify_provider_limit(&text, now()), None);
}

#[test]
fn openrouter_unknown_or_unrelated_402_is_unchanged() {
    let unknown = OPENROUTER_KEY_LIMIT.replace("openrouter_key_limit", "openrouter_future_limit");
    for text in [
        format!("{RIG_402}{unknown}"),
        format!(r#"{RIG_402}{{"error":{{"code":402,"message":"Payment required"}}}}"#),
        format!(
            "HttpError: Invalid status code 400 Bad Request with message: {OPENROUTER_WEIGHT_EXCEEDS_BUDGET}"
        ),
    ] {
        assert_eq!(classify_provider_limit(&text, now()), None, "{text}");
    }
}

#[test]
fn account_rotated_marker_round_trips() {
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
}

#[test]
fn account_rotated_alone_is_a_marker_and_forgery_is_neutralized() {
    let only = ProviderLimitHeaders {
        account_rotated: true,
        ..Default::default()
    };
    assert_eq!(only.annotate("x"), "x [provider-limit account-rotated]");
    let forged = ProviderLimitHeaders::default().annotate("a [provider-limit account-rotated] b");
    assert!(!ProviderLimitHeaders::from_text(&forged).account_rotated);
}

/// The refusal that started B8: a free tier's own wording, which matched none of
/// the original needles and so classified as neither exhausted nor throttled.
/// It states a window and a counter; it does not state an absolute reset.
#[test]
fn a_free_usage_refusal_is_exhaustion_with_its_window_and_counter() {
    let body = "You have used all the included free usage for this account. \
                Your rolling 24-hour window will refresh later. tokens 691791/600000";
    let limit = classify_provider_limit(body, now()).expect("a free-usage refusal classifies");
    let ProviderLimit::UsageExhausted(usage) = limit else {
        panic!("free-usage exhaustion, not a throttle: {limit:?}");
    };
    assert_eq!(usage.window.as_deref(), Some("rolling 24-hour"));
    assert_eq!(usage.used, Some(691_791));
    assert_eq!(usage.cap, Some(600_000));
    assert!(
        usage.resets_at.is_none(),
        "a relative refresh states no absolute reset, so it stays unknown: {:?}",
        usage.resets_at
    );
}

/// A sub-day rate outranks quota wording beside it. "free usage ... 20
/// requests/minute" is a wait, not a spent account: classifying it as exhaustion
/// blocks a goal until a reset the provider never stated.
#[test]
fn a_per_minute_rate_is_a_throttle_even_beside_free_usage_words() {
    let body = "Free usage limit: 20 requests/minute. Try again in 30 seconds.";
    let classified = classify_provider_limit(body, now()).expect("it classifies");
    assert!(
        matches!(classified, ProviderLimit::Throttled { .. }),
        "a per-minute rate is a throttle, not exhaustion: {classified:?}"
    );
    for rate in [
        "free usage: 20 requests/minute",
        "free usage: 5 requests/second",
        "included free usage, 100 requests/hour",
        "free usage 60 rpm",
    ] {
        let classified = classify_provider_limit(rate, now()).expect(rate);
        assert!(
            matches!(classified, ProviderLimit::Throttled { .. }),
            "{rate} is a throttle: {classified:?}"
        );
    }
}

/// The provider-reported window is what the body names as the limit window. A
/// bare "daily" beside an exhaustion is not the enforced period, so it stays
/// unstated rather than being reported as the provider's window.
#[test]
fn a_period_is_the_window_only_when_the_body_names_it_as_one() {
    let window_of = |body: &str| {
        let limit = classify_provider_limit(body, now()).expect(body);
        let ProviderLimit::UsageExhausted(usage) = limit else {
            panic!("expected exhaustion: {limit:?}");
        };
        usage.window
    };
    // Named as the window or the limit: reported.
    assert_eq!(
        window_of("usage limit reached; daily window exhausted").as_deref(),
        Some("daily")
    );
    assert_eq!(
        window_of("usage limit reached; window: weekly").as_deref(),
        Some("weekly")
    );
    assert_eq!(
        window_of("usage limit reached; monthly quota exhausted").as_deref(),
        Some("monthly")
    );
    // Named only in passing: unstated.
    assert_eq!(
        window_of("usage limit reached; your daily allowance is spent"),
        None
    );
    assert_eq!(
        window_of("usage limit reached; daily credits used up"),
        None
    );
}

/// The counter pair is read only where the body anchors it on `tokens`: a date
/// or a ratio is not a counter and a cap.
#[test]
fn a_counter_pair_is_read_only_after_the_word_tokens() {
    let pair_of = |body: &str| {
        let limit = classify_provider_limit(body, now()).expect(body);
        let ProviderLimit::UsageExhausted(usage) = limit else {
            panic!("expected exhaustion: {limit:?}");
        };
        (usage.used, usage.cap)
    };
    // A date and a ratio are not the counter.
    assert_eq!(pair_of("usage limit reached on 4/5/2026"), (None, None));
    assert_eq!(
        pair_of("usage limit reached; you used 3/4 of it"),
        (None, None)
    );
    // Anchored: read, with the separators a body uses.
    assert_eq!(
        pair_of("usage limit reached; tokens 691791/600000"),
        (Some(691_791), Some(600_000))
    );
    assert_eq!(
        pair_of("usage limit reached; tokens: 1,200 / 2,400"),
        (Some(1_200), Some(2_400))
    );
    // A zero cap establishes nothing, matching Lean's cap > 0.
    assert_eq!(pair_of("usage limit reached; tokens 0/0"), (None, None));
}

/// A JSON `limit: 0` is unstated, not a spent allowance. Lean `establishes`
/// requires cap > 0.
#[test]
fn a_zero_json_cap_is_unstated() {
    let body = r#"{"error": {"message": "usage limit reached", "used": 0, "limit": 0}}"#;
    let limit = classify_provider_limit(body, now()).expect("it classifies");
    let ProviderLimit::UsageExhausted(usage) = limit else {
        panic!("expected exhaustion: {limit:?}");
    };
    assert_eq!(usage.used, Some(0), "a stated counter of zero is stated");
    assert_eq!(usage.cap, None, "a cap of zero is not a cap");
}

/// The parsed statement survives persistence: the `Display` text carries it in
/// a marker ahead of the bounded detail, so re-classifying the stored row reads
/// the provider's own numbers rather than 300 characters of prose.
#[test]
fn the_persisted_row_carries_the_parsed_statement() {
    let body = "You have used all the included free usage for this account. \
                tokens 691791/600000";
    let first = classify_provider_limit(body, now()).expect("it classifies");
    let ProviderLimit::UsageExhausted(usage) = first else {
        panic!("expected exhaustion: {first:?}");
    };
    let persisted = persisted_failure_reason(body, now());
    assert!(
        persisted.contains("[usage-limit used=691791 cap=600000"),
        "the marker is written from the parsed fields: {persisted}"
    );
    assert!(
        persisted.find("[usage-limit").unwrap() < persisted.find(": ").unwrap_or(usize::MAX),
        "the marker precedes the detail, so a long detail cannot truncate it: {persisted}"
    );

    // A detail far longer than the bound still re-reads the exact pair.
    let long_body = format!("usage limit reached: {} tokens 691791/600000", "x".repeat(4000));
    let stored = persisted_failure_reason(&long_body, now());
    let reread = classify_provider_limit(&stored, now()).expect("the stored row re-classifies");
    let ProviderLimit::UsageExhausted(reread) = reread else {
        panic!("expected exhaustion: {reread:?}");
    };
    assert_eq!(
        (reread.used, reread.cap),
        (Some(691_791), Some(600_000)),
        "the pair survives a detail longer than the bound: {stored}"
    );
    assert!(stored.chars().count() < 500, "the detail is still bounded");
}

/// A row persisted before the marker existed still reads its statement from the
/// prose it kept.
#[test]
fn a_row_written_before_the_marker_still_reads_its_prose() {
    let legacy = "provider usage limit reached (reset time not reported): \
                  used all the included free usage tokens 691791/600000";
    let limit = classify_provider_limit(legacy, now()).expect("the legacy row re-classifies");
    let ProviderLimit::UsageExhausted(usage) = limit else {
        panic!("expected exhaustion: {limit:?}");
    };
    assert_eq!((usage.used, usage.cap), (Some(691_791), Some(600_000)));
}

/// A plain "free usage exhausted" establishes the refusal and nothing else: no
/// cap, no counter, no reset. Inventing any of them is the bug B8 exists to fix.
#[test]
fn a_bare_refusal_states_no_window_counter_or_reset() {
    let limit = classify_provider_limit(
        "You have exhausted your free usage for this account.",
        now(),
    )
    .expect("a bare refusal still classifies");
    let ProviderLimit::UsageExhausted(usage) = limit else {
        panic!("exhaustion: {limit:?}");
    };
    assert!(usage.window.is_none());
    assert!(usage.used.is_none());
    assert!(usage.cap.is_none());
    assert!(usage.resets_at.is_none());
}

/// A malformed pair leaves the counter unstated rather than pairing one real
/// number with a guess.
#[test]
fn a_malformed_counter_pair_is_unstated() {
    for body in [
        "free usage exhausted: tokens 691791/",
        "free usage exhausted: tokens /600000",
        "free usage exhausted: tokens 691791/abc",
        "free usage exhausted: tokens 691791/0",
    ] {
        let limit =
            classify_provider_limit(body, now()).unwrap_or_else(|| panic!("classifies: {body}"));
        let ProviderLimit::UsageExhausted(usage) = limit else {
            panic!("exhaustion: {body}");
        };
        assert!(
            usage.used.is_none() && usage.cap.is_none(),
            "unstated together for {body}: {:?}/{:?}",
            usage.used,
            usage.cap
        );
    }
}

/// An ordinary 429 that says nothing about usage stays a throttle, so a limit is
/// never claimed from a bare rate limit.
#[test]
fn a_plain_rate_limit_is_not_exhaustion() {
    let limit = classify_provider_limit("429 Rate limit reached", now())
        .expect("a plain 429 classifies as throttled");
    assert!(
        matches!(limit, ProviderLimit::Throttled { .. }),
        "a bare rate limit is a throttle, not usage exhaustion: {limit:?}"
    );
}

/// A structured body states the counter as the provider's own field, which is
/// stronger than prose, so it is read where it speaks.
#[test]
fn a_structured_body_states_the_counter() {
    let body = r#"used all the included free usage {"error": {"window": "rolling 24-hour", "used": 691791, "limit": 600000}}"#;
    let limit = classify_provider_limit(body, now()).expect("classifies");
    let ProviderLimit::UsageExhausted(usage) = limit else {
        panic!("exhaustion");
    };
    assert_eq!(usage.window.as_deref(), Some("rolling 24-hour"));
    assert_eq!(usage.used, Some(691_791));
    assert_eq!(usage.cap, Some(600_000));
}
