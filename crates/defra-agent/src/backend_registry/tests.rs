use super::*;
use crate::backend_provider::BackendProviderKind;

#[test]
fn inference_backend_from_value_parses() {
    let json = serde_json::json!({
        "backend_id": "workstation-dual",
        "name": "Workstation Dual GPU",
        "provider_kind": "OpenRouter",
        "endpoint": "http://100.73.235.38:8000/v1",
        "api_key": "raw-key",
        "api_key_env_var": "DUAL_GPU_API_KEY",
        "max_concurrent": 4,
        "max_queue_depth": 9,
        "enabled": true,
        "supports_tool_calls": false,
        "supports_streaming": false,
        "supports_structured_outputs": true,
        "supports_json_schema": true,
        "models": ["openrouter/auto", "anthropic/claude-3.7-sonnet"],
        "probe_status": "healthy",
    });

    let backend = InferenceBackend::from_value(&json).expect("should parse");
    assert_eq!(backend.backend_id, "workstation-dual");
    assert_eq!(backend.provider_kind, BackendProviderKind::OpenRouter);
    assert_eq!(backend.endpoint, "http://100.73.235.38:8000/v1");
    assert_eq!(backend.api_key.as_deref(), Some("raw-key"));
    assert_eq!(backend.api_key_env_var.as_deref(), Some("DUAL_GPU_API_KEY"));
    assert_eq!(backend.max_concurrent, 4);
    assert_eq!(backend.max_queue_depth, 9);
    assert!(backend.enabled);
    assert!(!backend.supports_tool_calls);
    assert!(!backend.supports_streaming);
    assert!(backend.supports_structured_outputs);
    assert!(backend.supports_json_schema);
    assert_eq!(
        backend.models,
        vec![
            "openrouter/auto".to_string(),
            "anthropic/claude-3.7-sonnet".to_string()
        ]
    );
    assert_eq!(backend.probe_status, "healthy");
}

#[test]
fn inference_backend_from_value_missing_fields_defaults() {
    let json = serde_json::json!({
        "backend_id": "test",
        "name": "Test",
        "endpoint": "http://localhost:8000/v1",
        "max_concurrent": 1,
        "enabled": true,
    });

    let backend = InferenceBackend::from_value(&json).expect("should parse");
    assert_eq!(backend.provider_kind, BackendProviderKind::OpenAiCompatible);
    assert_eq!(backend.api_key, None);
    assert_eq!(backend.api_key_env_var, None);
    assert_eq!(backend.max_queue_depth, DEFAULT_MAX_QUEUE_DEPTH);
    assert!(backend.supports_tool_calls);
    assert!(backend.supports_streaming);
    assert!(!backend.supports_structured_outputs);
    assert!(!backend.supports_json_schema);
    assert!(backend.models.is_empty());
    assert_eq!(backend.probe_status, "unknown");
}

#[test]
fn is_available_requires_enabled_and_healthy() {
    let healthy = InferenceBackend {
        backend_id: "test".into(),
        name: "Test".into(),
        provider_kind: BackendProviderKind::OpenAiCompatible,
        endpoint: "http://localhost:8000/v1".into(),
        api_key: None,
        api_key_env_var: None,
        max_concurrent: 1,
        max_queue_depth: DEFAULT_MAX_QUEUE_DEPTH,
        enabled: true,
        supports_tool_calls: true,
        supports_streaming: true,
        supports_structured_outputs: false,
        supports_json_schema: false,
        models: Vec::new(),
        probe_status: "healthy".into(),
    };
    assert!(healthy.is_available());

    let disabled = InferenceBackend {
        enabled: false,
        ..healthy.clone()
    };
    assert!(!disabled.is_available());

    let unhealthy = InferenceBackend {
        probe_status: "unhealthy".into(),
        ..healthy.clone()
    };
    assert!(!unhealthy.is_available());
}

#[test]
fn try_acquire_respects_capacity() {
    let tracker = BackendTracker::new();

    assert_eq!(tracker.running_count("b1"), 0);
    assert!(tracker.try_acquire("b1", 2));
    assert_eq!(tracker.running_count("b1"), 1);

    assert!(tracker.try_acquire("b1", 2));
    assert_eq!(tracker.running_count("b1"), 2);

    assert!(!tracker.try_acquire("b1", 2));
    assert_eq!(tracker.running_count("b1"), 2);

    tracker.release("b1");
    assert_eq!(tracker.running_count("b1"), 1);

    assert!(tracker.try_acquire("b1", 2));
}

#[test]
fn release_floors_at_zero() {
    let tracker = BackendTracker::new();
    tracker.release("nonexistent");
    assert_eq!(tracker.running_count("nonexistent"), 0);
}

#[test]
fn backend_permit_releases_on_drop() {
    let tracker = Arc::new(BackendTracker::new());

    {
        let _permit = tracker
            .try_acquire_permit("b1", 1)
            .expect("permit should be acquired");
        assert_eq!(tracker.running_count("b1"), 1);
    }

    assert_eq!(tracker.running_count("b1"), 0);
}
