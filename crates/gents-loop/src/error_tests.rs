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
