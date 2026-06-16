use super::*;

#[test]
fn inference_error_retryability() {
    assert!(InferenceError::ModelUnreachable {
        endpoint: "http://localhost:8000/v1".into()
    }
    .is_retryable());

    assert!(InferenceError::TransientFailure {
        reason: "connection reset".into()
    }
    .is_retryable());

    assert!(!InferenceError::PermanentFailure {
        reason: "invalid_api_key".into()
    }
    .is_retryable());

    assert!(InferenceError::RateLimited {
        retry_after_secs: 30
    }
    .is_retryable());

    assert!(!InferenceError::ContextLengthExceeded {
        reason: "too long".into()
    }
    .is_retryable());

    assert!(!InferenceError::RetriesExhausted {
        max_retries: 3,
        last_error: "gone".into()
    }
    .is_retryable());
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
fn openai_compatible_404_is_permanent_backend_configuration() {
    let error =
        rig::agent::StreamingError::Completion(rig::completion::CompletionError::ProviderError(
            "InvalidStatusCodeWithMessage(404, \"\")".into(),
        ));

    let classified = classify_completion_error(&error);

    assert!(matches!(
        classified,
        InferenceError::PermanentFailure { .. }
    ));
    assert!(
        !classified.is_retryable(),
        "missing /chat/completions route should fail fast instead of retrying"
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
fn provider_transport_connection_reset_is_retryable_even_with_auth_like_url_digits() {
    let error =
        rig::agent::StreamingError::Completion(rig::completion::CompletionError::ProviderError(
            "Http client error: connection reset by peer while sending request to \
             http://127.0.0.1:4010/v1/chat/completions"
                .into(),
        ));

    let classified = classify_completion_error(&error);

    assert!(matches!(
        classified,
        InferenceError::TransientFailure { .. }
    ));
    assert!(
        classified.is_retryable(),
        "connection resets should retry even when the URL contains 401-like digits"
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
fn vllm_tool_call_json_parse_400_is_retryable() {
    // vLLM's tool-call parser fails to json.loads the model's streamed tool arguments
    // (here a missing delimiter deep in an array-typed argument) and returns 400. This
    // is intermittent/sampling-dependent, so it should retry rather than fail fast.
    let error =
        rig::agent::StreamingError::Completion(rig::completion::CompletionError::ProviderError(
            "Invalid status code 400 Bad Request with message: {\"error\":{\"message\":\
             \"Expecting ',' delimiter: line 8 column 61 (char 4982)\",\"type\":\
             \"BadRequestError\",\"param\":null,\"code\":400}}"
                .into(),
        ));

    let classified = classify_completion_error(&error);

    assert!(matches!(
        classified,
        InferenceError::TransientFailure { .. }
    ));
    assert!(
        classified.is_retryable(),
        "intermittent vLLM tool-call JSON parse 400s should retry on a fresh generation"
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
fn provider_400_without_json_decode_signature_stays_permanent() {
    // Regression guard: a genuine request-shape 400 lacks the JSONDecodeError signature
    // and must remain a permanent failure, unaffected by the tool-parse retry carve-out.
    let error =
        rig::agent::StreamingError::Completion(rig::completion::CompletionError::ProviderError(
            "Invalid status code 400 Bad Request with message: {\"error\":{\"message\":\
             \"duplicate field `max_tokens`\",\"type\":\"BadRequestError\"}}"
                .into(),
        ));

    let classified = classify_completion_error(&error);

    assert!(matches!(
        classified,
        InferenceError::PermanentFailure { .. }
    ));
    assert!(!classified.is_retryable());
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
fn provider_error_precise_429_is_rate_limited() {
    let error =
        rig::agent::StreamingError::Completion(rig::completion::CompletionError::ProviderError(
            "Invalid status code 429: too many requests".into(),
        ));

    let classified = classify_completion_error(&error);

    assert!(matches!(classified, InferenceError::RateLimited { .. }));
    assert!(classified.is_retryable());
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
fn daemon_error_from_variants() {
    let config_err: DaemonError = ConfigError::Missing {
        key: "backend_endpoint".into(),
    }
    .into();
    assert!(matches!(config_err, DaemonError::Config(_)));

    let watcher_err: DaemonError = WatcherError::EventBusClosed.into();
    assert!(matches!(watcher_err, DaemonError::Watcher(_)));
}

#[test]
fn error_display_messages() {
    let err = InferenceError::RetriesExhausted {
        max_retries: 3,
        last_error: "timeout".into(),
    };
    assert!(err.to_string().contains("3 attempts"));

    let err = WatcherError::ClaimFailed {
        doc_id: "doc-1".into(),
        reason: "already processing".into(),
    };
    assert!(err.to_string().contains("doc-1"));
}

#[test]
fn client_tool_args_unparseable_signal_is_retryable() {
    // The completion loop synthesizes this provider-error message when a tool's
    // `arguments` string is unparseable even after a tolerant repair (a raw
    // lone-backslash escape, or a payload truncated by finish_reason=length).
    // It is the client-side analogue of vLLM's server-side tool-parse 400 and
    // must classify as a transient/retryable failure so the run re-attempts.
    let error = rig::agent::StreamingError::Completion(
        rig::completion::CompletionError::ProviderError(format!(
            "{}: tool 'post_status': tool args unparseable (truncated): EOF while parsing a string",
            CLIENT_TOOL_ARGS_UNPARSEABLE_MARKER
        )),
    );

    let classified = classify_completion_error(&error);

    assert!(matches!(
        classified,
        InferenceError::TransientFailure { .. }
    ));
    assert!(
        classified.is_retryable(),
        "client-side unparseable tool args should retry on a fresh generation"
    );
}
