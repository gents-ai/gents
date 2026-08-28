use std::collections::BTreeMap;
use std::sync::Arc;

use super::{
    request_is_authorized, validate_bind_security, CodexSidecar, ConnectionState,
    DEFAULT_MEMORY_MODE,
};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, HeaderValue};
use tokio::sync::{mpsc, watch, Mutex};

fn test_connection() -> ConnectionState {
    let (outbound, _outbound_rx) = mpsc::unbounded_channel::<String>();
    ConnectionState {
        outbound,
        turn_streams: Arc::new(Mutex::new(BTreeMap::new())),
        fuzzy_file_search_sessions: Arc::new(Mutex::new(BTreeMap::new())),
        pending_steering_inputs: Arc::new(Mutex::new(BTreeMap::new())),
        child_thread_streams: Arc::new(Mutex::new(BTreeMap::new())),
        root_continuation_streams: Arc::new(Mutex::new(BTreeMap::new())),
    }
}

#[test]
fn memory_mode_defaults_to_disabled_for_unknown_thread() {
    let sidecar = CodexSidecar::default();
    assert_eq!(sidecar.memory_mode_or_default("never-set"), "disabled");
    assert_eq!(DEFAULT_MEMORY_MODE, "disabled");
}

#[test]
fn memory_mode_returns_explicit_override_when_set() {
    let mut sidecar = CodexSidecar::default();
    sidecar
        .memory_mode
        .insert("t1".to_string(), "enabled".to_string());
    assert_eq!(sidecar.memory_mode_or_default("t1"), "enabled");
    assert_eq!(sidecar.memory_mode_or_default("t2"), "disabled");
}

#[test]
fn websocket_auth_requires_exact_bearer_token_when_configured() {
    let mut headers = HeaderMap::new();
    assert!(request_is_authorized(&headers, None));
    assert!(!request_is_authorized(&headers, Some("secret")));

    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer wrong"));
    assert!(!request_is_authorized(&headers, Some("secret")));

    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
    assert!(request_is_authorized(&headers, Some("secret")));
}

#[test]
fn non_loopback_bind_requires_authentication() {
    assert!(validate_bind_security("127.0.0.1".parse().unwrap(), None).is_ok());
    assert!(validate_bind_security("192.0.2.10".parse().unwrap(), None).is_err());
    assert!(validate_bind_security("192.0.2.10".parse().unwrap(), Some("secret")).is_ok());
    assert!(validate_bind_security("0.0.0.0".parse().unwrap(), Some("secret")).is_err());
}

#[tokio::test]
async fn replacing_root_watcher_clears_only_its_owned_turn_generation() {
    let connection = test_connection();
    let old_task = tokio::spawn(std::future::pending::<()>());
    connection
        .replace_root_continuation_stream(
            "thread-1".to_string(),
            "watcher-old".to_string(),
            old_task.abort_handle(),
        )
        .await;

    let (interactive_tx, _) = watch::channel(false);
    let interactive = super::turn::install_stream_control(
        &connection,
        "thread-1".to_string(),
        "interactive".to_string(),
        None,
        interactive_tx,
    )
    .await;
    let (old_tx, _) = watch::channel(false);
    let old = super::turn::install_stream_control(
        &connection,
        "thread-1".to_string(),
        "wake-1".to_string(),
        Some("watcher-old"),
        old_tx,
    )
    .await;
    let (new_tx, _) = watch::channel(false);
    let new = super::turn::install_stream_control(
        &connection,
        "thread-1".to_string(),
        "wake-1".to_string(),
        Some("watcher-new"),
        new_tx,
    )
    .await;

    let new_task = tokio::spawn(std::future::pending::<()>());
    connection
        .replace_root_continuation_stream(
            "thread-1".to_string(),
            "watcher-new".to_string(),
            new_task.abort_handle(),
        )
        .await;
    drop(old);

    assert!(connection.has_turn_stream("thread-1", "interactive").await);
    assert!(connection.has_turn_stream("thread-1", "wake-1").await);

    connection.stop_root_continuation_stream("thread-1").await;
    assert!(connection.has_turn_stream("thread-1", "interactive").await);
    assert!(!connection.has_turn_stream("thread-1", "wake-1").await);

    interactive.clear().await;
    new.clear().await;
}
