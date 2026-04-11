use super::*;

#[test]
fn inference_backend_from_value_parses() {
    let json = serde_json::json!({
        "backend_id": "workstation-dual",
        "name": "Workstation Dual GPU",
        "endpoint": "http://100.73.235.38:8000/v1",
        "api_key_env_var": "DUAL_GPU_API_KEY",
        "max_concurrent": 4,
        "enabled": true,
        "probe_status": "healthy",
    });

    let backend = InferenceBackend::from_value(&json).expect("should parse");
    assert_eq!(backend.backend_id, "workstation-dual");
    assert_eq!(backend.endpoint, "http://100.73.235.38:8000/v1");
    assert_eq!(backend.api_key_env_var.as_deref(), Some("DUAL_GPU_API_KEY"));
    assert_eq!(backend.max_concurrent, 4);
    assert!(backend.enabled);
    assert_eq!(backend.probe_status, "healthy");
}

#[test]
fn inference_backend_from_value_missing_probe_status_defaults() {
    let json = serde_json::json!({
        "backend_id": "test",
        "name": "Test",
        "endpoint": "http://localhost:8000/v1",
        "max_concurrent": 1,
        "enabled": true,
    });

    let backend = InferenceBackend::from_value(&json).expect("should parse");
    assert_eq!(backend.probe_status, "unknown");
}

#[test]
fn is_available_requires_enabled_and_healthy() {
    let healthy = InferenceBackend {
        backend_id: "test".into(),
        name: "Test".into(),
        endpoint: "http://localhost:8000/v1".into(),
        api_key_env_var: None,
        max_concurrent: 1,
        enabled: true,
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
