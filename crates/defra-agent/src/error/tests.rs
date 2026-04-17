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
