use super::*;

fn classify_completion_error(error: &rig::agent::StreamingError) -> InferenceError {
    crate::rig_compat::classify_completion_error(error).error
}

#[test]
fn tool_streaming_errors_are_permanent_until_retry_metadata_exists() {
    let error = rig::agent::StreamingError::Tool(rig::tool::ToolSetError::ToolNotFoundError(
        "missing_tool".into(),
    ));

    let classified = classify_completion_error(&error);

    assert!(matches!(
        classified,
        InferenceError::PermanentFailure { .. }
    ));
    assert!(
        !classified.is_retryable(),
        "tool failures stay permanent until tools expose retry-safe health/idempotency metadata"
    );
}

#[test]
fn provider_transport_send_failure_is_retryable_even_with_status_like_url_digits() {
    let error =
        rig::agent::StreamingError::Completion(rig::completion::CompletionError::ProviderError(
            "Http client error: error sending request for url \
             (http://127.0.0.1:4000/v1/chat/completions)"
                .into(),
        ));

    let classified = classify_completion_error(&error);

    assert!(matches!(
        classified,
        InferenceError::TransientFailure { .. }
    ));
    assert!(
        classified.is_retryable(),
        "transport send failures should retry even when the URL contains 400-like digits"
    );
}

#[test]
fn provider_error_uses_precise_status_matching_for_permanent_statuses() {
    let error =
        rig::agent::StreamingError::Completion(rig::completion::CompletionError::ProviderError(
            "InvalidStatusCodeWithMessage(400, \"duplicate field `max_tokens`\")".into(),
        ));

    let classified = classify_completion_error(&error);

    assert!(matches!(
        classified,
        InferenceError::PermanentFailure { .. }
    ));
    assert!(
        !classified.is_retryable(),
        "precise provider 400 statuses are still permanent request-shape failures"
    );
}

#[test]
fn vllm_tool_call_invalid_escape_400_is_retryable() {
    // The `Invalid \escape` variant — the wire body double-escapes the backslash, which
    // is why the matcher keys off the line/column/(char ...) signature, not the phrase.
    let error =
        rig::agent::StreamingError::Completion(rig::completion::CompletionError::ProviderError(
            "Invalid status code 400 Bad Request with message: {\"error\":{\"message\":\
             \"Invalid \\\\escape: line 1 column 34 (char 33)\",\"type\":\"BadRequestError\"}}"
                .into(),
        ));

    let classified = classify_completion_error(&error);

    assert!(matches!(
        classified,
        InferenceError::TransientFailure { .. }
    ));
    assert!(classified.is_retryable());
}

#[test]
fn provider_error_loose_status_digits_do_not_force_permanent_failure() {
    let error =
        rig::agent::StreamingError::Completion(rig::completion::CompletionError::ProviderError(
            "upstream worker closed after writing 400 bytes".into(),
        ));

    let classified = classify_completion_error(&error);

    assert!(matches!(
        classified,
        InferenceError::TransientFailure { .. }
    ));
    assert!(
        classified.is_retryable(),
        "plain numeric substrings must not be treated as HTTP status codes"
    );
}

#[test]
fn openai_compatible_http_400_is_permanent_bad_request() {
    let error =
        rig::agent::StreamingError::Completion(rig::completion::CompletionError::HttpError(
            rig::http_client::Error::InvalidStatusCodeWithMessage(
                "400".parse().expect("valid status"),
                "duplicate field `max_tokens`".into(),
            ),
        ));

    let classified = classify_completion_error(&error);

    assert!(matches!(
        classified,
        InferenceError::PermanentFailure { .. }
    ));
    assert!(
        !classified.is_retryable(),
        "bad OpenAI-compatible request bodies should fail fast instead of retrying"
    );
}

#[test]
fn anthropic_account_rate_limit_is_a_non_retryable_usage_limit() {
    let error =
        rig::agent::StreamingError::Completion(rig::completion::CompletionError::ProviderError(
            r#"Claude Messages HTTP 429 Too Many Requests (request-id -) body={"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed your account's rate limit. Please try again later."}} [provider-limit reset-at=2026-09-25T18:40:00Z usage-exhausted]"#.into(),
        ));

    let classified = classify_completion_error(&error);

    assert!(!classified.is_retryable());
    assert_eq!(
        classified.to_string(),
        "provider usage limit reached (resets at 2026-09-25T18:40:00Z): \
         This request would exceed your account's rate limit. Please try again later."
    );
}

fn http_402(body: String) -> crate::error::ClassifiedFailure {
    crate::rig_compat::classify_completion_error(&rig::agent::StreamingError::Completion(
        rig::completion::CompletionError::HttpError(
            rig::http_client::Error::InvalidStatusCodeWithMessage(
                "402".parse().expect("valid status"),
                body,
            ),
        ),
    ))
}

#[test]
fn openrouter_credits_402_is_a_non_retryable_usage_limit() {
    use crate::completion_retry::{failure_class, FailureClass};
    use crate::provider_limit::tests::{
        OPENROUTER_INSUFFICIENT_CREDITS, OPENROUTER_KEY_LIMIT, OPENROUTER_WEIGHT_EXCEEDS_BUDGET,
    };
    for body in [
        OPENROUTER_INSUFFICIENT_CREDITS,
        OPENROUTER_KEY_LIMIT,
        OPENROUTER_WEIGHT_EXCEEDS_BUDGET,
    ] {
        let failure = http_402(body.to_string());
        let error = &failure.error;
        assert!(
            matches!(error, InferenceError::UsageLimited(_)),
            "{body}: {error:?}"
        );
        assert!(!error.is_retryable());
        assert_eq!(failure_class(&failure), FailureClass::Permanent);
    }
}

#[test]
fn openrouter_in_flight_402_waits_for_retry_after() {
    use crate::completion_retry::{failure_class, FailureClass};
    let retry_at = (chrono::Utc::now() + chrono::Duration::seconds(20))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let failure = http_402(format!(
        "{} [provider-limit retry-at={retry_at}]",
        crate::provider_limit::tests::OPENROUTER_IN_FLIGHT
    ));
    match &failure.error {
        InferenceError::RateLimited {
            retry_after: Some(wait),
        } => assert!(
            *wait <= std::time::Duration::from_secs(20)
                && *wait >= std::time::Duration::from_secs(18),
            "{wait:?}"
        ),
        other => panic!("expected throttle, got {other:?}"),
    }
    assert_eq!(failure_class(&failure), FailureClass::Transport);
}

#[test]
fn model_refusal_is_a_non_retryable_permanent_failure() {
    // The Claude Messages parser surfaces a content-free refusal as a provider
    // error carrying the "stop_reason: refusal" marker. Resending the identical
    // body earns the identical refusal, so the loop must fail fast rather than
    // resample and burn the subscription's quota.
    let error =
        rig::agent::StreamingError::Completion(rig::completion::CompletionError::ProviderError(
            "the model declined to respond (stop_reason: refusal) (request-id req_abc)".into(),
        ));

    let classified = classify_completion_error(&error);

    assert!(matches!(
        classified,
        InferenceError::PermanentFailure { .. }
    ));
    assert!(
        !classified.is_retryable(),
        "a model refusal is deterministic and must not be resampled"
    );
}

#[test]
fn http_status_rig_carries_decides_over_status_text() {
    let error =
        rig::agent::StreamingError::Completion(rig::completion::CompletionError::HttpError(
            rig::http_client::Error::InvalidStatusCodeWithMessage(
                "503".parse().expect("valid status"),
                "upstream relayed: Invalid status code 400".into(),
            ),
        ));
    assert!(matches!(
        classify_completion_error(&error),
        InferenceError::TransientFailure { .. }
    ));
}

#[test]
fn loop_verdicts_classify_from_their_variant_not_their_text() {
    let verdict = crate::rig_compat::loop_verdict_error(
        crate::error::LoopVerdict::NoVisibleOutput,
        "completion produced no visible output: maximum context length".into(),
    );
    assert!(matches!(
        verdict.classify(),
        InferenceError::TransientFailure { .. }
    ));
    assert!(crate::loop_stream::is_no_visible_output_failure(&verdict));
    assert_eq!(verdict.provider_message(), None);
}

/// The live Claude Messages rejection, as the transport renders it.
pub(crate) const CLAUDE_PROMPT_TOO_LONG: &str = r#"Claude Messages HTTP 400 Bad Request (request-id -) body={"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 1001685 tokens > 1000000 maximum"},"request_id":"req_011CgVVYfxcw64a1gDiAAhYC"}"#;

/// Claude's rejection when the input fits but the requested output does not.
pub(crate) const CLAUDE_INPUT_AND_MAX_TOKENS_EXCEED_LIMIT: &str = r#"Claude Messages HTTP 400 Bad Request (request-id -) body={"type":"error","error":{"type":"invalid_request_error","message":"input length and `max_tokens` exceed context limit: 900000 + 128000 > 1000000, decrease input length or `max_tokens` and try again"},"request_id":"req_011CgVVYfxcw64a1gDiAAhYD"}"#;

fn provider_error(text: &str) -> rig::agent::StreamingError {
    rig::agent::StreamingError::Completion(rig::completion::CompletionError::ProviderError(
        text.into(),
    ))
}

#[test]
fn claude_prompt_too_long_is_a_context_overflow_with_its_counts() {
    let classified =
        crate::rig_compat::classify_completion_error(&provider_error(CLAUDE_PROMPT_TOO_LONG));
    assert_eq!(
        classified.report.context_overflow,
        Some(ContextOverflow {
            input_tokens: Some(1_001_685),
            limit: Some(1_000_000),
        })
    );
    match &classified.error {
        InferenceError::ContextLengthExceeded {
            reason,
            input_tokens,
            limit,
        } => {
            assert!(reason.contains("prompt is too long"), "{reason}");
            assert_eq!((*input_tokens, *limit), (Some(1_001_685), Some(1_000_000)));
        }
        other => panic!("expected a context overflow, got {other:?}"),
    }
    assert!(!classified.error.is_retryable());
}

#[test]
fn openai_and_xai_context_overflows_are_classified_with_their_counts() {
    for (text, input_tokens, limit) in [
        (
            r#"Invalid status code 400 Bad Request with message: {"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 130,532 tokens. Please reduce the length of the messages.","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}"#,
            Some(130_532),
            Some(128_000),
        ),
        (
            "This model's maximum context length is 8192 tokens, however you requested 9000 tokens",
            Some(9_000),
            Some(8_192),
        ),
        (
            r#"{"code":"Client specified an invalid argument","error":"This model's maximum prompt length is 131072 but the request contains 145000 tokens."}"#,
            Some(145_000),
            Some(131_072),
        ),
        (
            r#"{"error":{"code":"context_length_exceeded","message":"Your input exceeds the context window of this model."}}"#,
            None,
            None,
        ),
        ("input is too long for the requested model", None, None),
        (
            "This model's maximum context length is 128000 tokens. However, you requested 130000 tokens (125000 in the messages, 5000 in the completion). Please reduce the length of the messages or completion.",
            Some(125_000),
            Some(128_000),
        ),
        (
            CLAUDE_INPUT_AND_MAX_TOKENS_EXCEED_LIMIT,
            Some(900_000),
            Some(1_000_000),
        ),
    ] {
        let classified = crate::rig_compat::classify_completion_error(&provider_error(text));
        match &classified.error {
            InferenceError::ContextLengthExceeded {
                input_tokens: observed_input,
                limit: observed_limit,
                ..
            } => assert_eq!(
                (*observed_input, *observed_limit),
                (input_tokens, limit),
                "{text}"
            ),
            other => panic!("expected a context overflow for {text}, got {other:?}"),
        }
    }
}

#[test]
fn ordinary_invalid_requests_are_not_context_overflows() {
    for text in [
        r#"Claude Messages HTTP 400 Bad Request (request-id -) body={"type":"error","error":{"type":"invalid_request_error","message":"messages.1.content: tool_use ids must be unique"}}"#,
        r#"Invalid status code 400 Bad Request with message: {"error":{"message":"Invalid value for 'max_tokens': must be at least 1.","type":"invalid_request_error","code":"invalid_value"}}"#,
        "max_tokens: 200000 > 128000, which is the maximum allowed number of output tokens",
    ] {
        let classified = crate::rig_compat::classify_completion_error(&provider_error(text));
        assert_eq!(classified.report.context_overflow, None, "{text}");
        assert!(
            !matches!(
                classified.error,
                InferenceError::ContextLengthExceeded { .. }
            ),
            "{text}: {:?}",
            classified.error
        );
    }
}

#[test]
fn payload_too_large_413_is_not_a_context_overflow() {
    for error in [
        rig::agent::StreamingError::Completion(rig::completion::CompletionError::HttpError(
            rig::http_client::Error::InvalidStatusCodeWithMessage(
                "413".parse().expect("valid status"),
                r#"{"type":"error","error":{"type":"request_too_large","message":"Request exceeds the maximum allowed number of bytes."}}"#.into(),
            ),
        )),
        provider_error(
            r#"Claude Messages HTTP 413 Payload Too Large (request-id -) body={"type":"error","error":{"type":"request_too_large","message":"Request exceeds the maximum allowed number of bytes."}}"#,
        ),
    ] {
        let classified = crate::rig_compat::classify_completion_error(&error);
        assert_eq!(classified.report.context_overflow, None);
        assert!(
            !matches!(
                classified.error,
                InferenceError::ContextLengthExceeded { .. }
            ),
            "{:?}",
            classified.error
        );
    }
}
