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
