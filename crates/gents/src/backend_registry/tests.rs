use super::*;
use crate::admission::BackendAdmissionConfig;
use crate::backend_provider::BackendProviderKind;
use crate::lean_vocab_test::lean_backend_health_admission_cases;
use crate::OpenAiWireApi;

#[test]
fn inference_backend_from_value_parses() {
    let json = serde_json::json!({
        "backend_id": "workstation-dual",
        "name": "Workstation Dual GPU",
        "provider_kind": "OpenRouter",
        "openai_wire_api": "chat_completions",
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
    assert_eq!(
        backend.openai_wire_api,
        Some(OpenAiWireApi::ChatCompletions)
    );
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
fn inference_backend_from_value_requires_provider_kind() {
    let json = serde_json::json!({
        "backend_id": "test",
        "name": "Test",
        "endpoint": "http://localhost:8000/v1",
        "max_concurrent": 1,
        "enabled": true,
    });

    let error = InferenceBackend::from_value(&json).expect_err("provider kind is required");
    assert!(error.to_string().contains("provider kind is required"));
}

#[test]
fn generated_backend_health_admission_cases_match_registry_and_admission_policy() {
    let cases = lean_backend_health_admission_cases();
    assert_eq!(cases.len(), 7);

    for case in cases {
        let backend = InferenceBackend {
            backend_id: case.name.clone(),
            name: case.name.clone(),
            provider_kind: BackendProviderKind::OpenAiCompatible,
            openai_wire_api: None,
            endpoint: "http://localhost:8000/v1".into(),
            api_key: None,
            api_key_env_var: None,
            max_concurrent: 1,
            max_queue_depth: DEFAULT_MAX_QUEUE_DEPTH,
            enabled: case.enabled,
            models: Vec::new(),
            probe_status: case.probe_status.clone(),
        };
        let admission_config =
            BackendAdmissionConfig::from_backend(&backend).expect("valid backend config");

        assert_eq!(
            crate::admission::document_configured_from_fields(
                backend.enabled,
                &backend.probe_status
            ),
            case.expected_available,
            "{} document-configured availability drifted from Lean case",
            case.name
        );
        assert_eq!(
            admission_config.is_available(),
            case.expected_available,
            "{} admission availability drifted from Lean case",
            case.name
        );
        assert_eq!(
            case.admission_decision.as_str(),
            if case.expected_available {
                "available"
            } else {
                "unavailable"
            },
            "{}",
            case.name
        );
        assert!(
            case.observed_document_only,
            "{} must stay scoped to the observed backend document",
            case.name
        );
        assert!(
            !case.external_endpoint_freshness_claimed,
            "{} must not claim endpoint/provider freshness",
            case.name
        );
    }
}

/// Operator-UI projection of `(enabled, probe_status)`. Drives every Lean
/// witness through `derive_display_state` and asserts:
///   * the derivation is total (every witness maps to a known bucket);
///   * the `available` bucket coincides exactly with the Lean
///     `expected_available` verdict.
///
/// This is the bridge-snapshot consumer test for the
/// `backend-health.operatorUi` row of the feature matrix — registered in
/// `CoverageLedger.lean` and `conformance_consumers.rs`.
#[test]
fn display_state_matches_every_lean_backend_health_admission_case() {
    let cases = lean_backend_health_admission_cases();
    assert_eq!(
        cases.len(),
        7,
        "Lean witness count drifted from operator UI expectations"
    );

    for case in cases {
        let actual = derive_display_state(case.enabled, &case.probe_status);
        let expected = expected_display_state(case.enabled, &case.probe_status);
        assert_eq!(
            actual, expected,
            "case {} mapped probe_status {} to {} but expected {}",
            case.name, case.probe_status, actual, expected
        );

        let panel_says_available = actual == "available";
        assert_eq!(
            panel_says_available, case.expected_available,
            "case {} drifted from Lean availability witness",
            case.name
        );
    }

    fn expected_display_state(enabled: bool, probe_status: &str) -> &'static str {
        if !enabled {
            return "disabled";
        }
        match probe_status {
            "healthy" => "available",
            "unhealthy" => "unhealthy",
            "stale" => "stale",
            "rate_limited" => "rate-limited",
            "circuit_open" => "circuit-open",
            "unknown" => "unknown",
            _ => "unknown",
        }
    }
}

fn backend_with_keys(api_key: Option<&str>, env_var: Option<&str>) -> InferenceBackend {
    InferenceBackend {
        backend_id: "test".into(),
        name: "Test".into(),
        provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: None,
        endpoint: "http://localhost:8000/v1".into(),
        api_key: api_key.map(ToOwned::to_owned),
        api_key_env_var: env_var.map(ToOwned::to_owned),
        max_concurrent: 1,
        max_queue_depth: DEFAULT_MAX_QUEUE_DEPTH,
        enabled: true,
        models: Vec::new(),
        probe_status: "unknown".into(),
    }
}

#[test]
fn resolved_api_key_prefers_raw_key() {
    let backend = backend_with_keys(Some("raw-key"), Some("BACKEND_REGISTRY_TEST_KEY_UNUSED"));
    assert_eq!(backend.resolved_api_key().as_deref(), Some("raw-key"));
}

#[test]
fn resolved_api_key_falls_back_to_env() {
    let var = "BACKEND_REGISTRY_TEST_KEY_FALLBACK";
    std::env::set_var(var, "env-key");
    let backend = backend_with_keys(None, Some(var));
    assert_eq!(backend.resolved_api_key().as_deref(), Some("env-key"));
    std::env::remove_var(var);
}

#[test]
fn resolved_api_key_none_when_unset() {
    let backend = backend_with_keys(None, None);
    assert_eq!(backend.resolved_api_key(), None);
    // Env var named but unset in the environment.
    let backend = backend_with_keys(None, Some("BACKEND_REGISTRY_TEST_KEY_MISSING"));
    assert_eq!(backend.resolved_api_key(), None);
}

// ---------------------------------------------------------------------------
// InferenceBackend::validate (#1331) — table-driven from the historical
// gents-cli desired-state rules (crates/gents-cli/src/desired_state/validate/agent.rs).
// ---------------------------------------------------------------------------

fn base_backend() -> InferenceBackend {
    InferenceBackend {
        backend_id: "reviewers".to_string(),
        name: "Reviewers".to_string(),
        provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: None,
        endpoint: "http://127.0.0.1:8000/v1".to_string(),
        api_key: None,
        api_key_env_var: None,
        max_concurrent: 4,
        max_queue_depth: 8,
        enabled: true,
        models: vec!["d4f".to_string()],
        probe_status: UNKNOWN_PROBE_STATUS.to_string(),
    }
}

#[test]
fn inference_backend_validate_accepts_a_well_formed_backend() {
    assert!(base_backend().validate(None).is_ok());
}

#[test]
fn inference_backend_validate_rejects_empty_backend_id() {
    let mut backend = base_backend();
    backend.backend_id = "  ".to_string();
    let error = backend.validate(None).unwrap_err().to_string();
    assert!(error.contains("backend_id must not be empty"), "{error}");
}

#[test]
fn inference_backend_validate_rejects_empty_endpoint() {
    let mut backend = base_backend();
    backend.endpoint = "  ".to_string();
    let error = backend.validate(None).unwrap_err().to_string();
    assert!(error.contains("endpoint must not be empty"), "{error}");
}

#[test]
fn inference_backend_validate_rejects_empty_api_key() {
    let mut backend = base_backend();
    backend.api_key = Some("   ".to_string());
    let error = backend.validate(None).unwrap_err().to_string();
    assert!(error.contains("api_key must not be empty"), "{error}");
}

#[test]
fn inference_backend_validate_rejects_api_key_and_env_var_together() {
    let mut backend = base_backend();
    backend.api_key = Some("sk-live".to_string());
    backend.api_key_env_var = Some("BACKEND_API_KEY".to_string());
    let error = backend.validate(None).unwrap_err().to_string();
    assert!(
        error.contains("must not set both api_key and api_key_env_var"),
        "{error}"
    );
}

#[test]
fn inference_backend_validate_accepts_api_key_xor_env_var() {
    let mut with_key = base_backend();
    with_key.api_key = Some("sk-live".to_string());
    assert!(with_key.validate(None).is_ok());

    let mut with_env_var = base_backend();
    with_env_var.api_key_env_var = Some("BACKEND_API_KEY".to_string());
    assert!(with_env_var.validate(None).is_ok());
}

#[test]
fn inference_backend_validate_rejects_non_positive_max_concurrent() {
    for value in [0, -1] {
        let mut backend = base_backend();
        backend.max_concurrent = value;
        let error = backend.validate(None).unwrap_err().to_string();
        assert!(error.contains("max_concurrent must be positive"), "{error}");
    }
}

#[test]
fn inference_backend_validate_rejects_non_positive_max_queue_depth() {
    for value in [0, -1] {
        let mut backend = base_backend();
        backend.max_queue_depth = value;
        let error = backend.validate(None).unwrap_err().to_string();
        assert!(
            error.contains("max_queue_depth must be positive"),
            "{error}"
        );
    }
}

#[test]
fn inference_backend_validate_no_lockout_rejects_dropping_the_current_model() {
    let backend = base_backend(); // models: ["d4f"]
    let error = backend.validate(Some("gone")).unwrap_err().to_string();
    assert!(
        error.contains("would drop the current model \"gone\""),
        "{error}"
    );
}

#[test]
fn inference_backend_validate_no_lockout_accepts_the_current_model_still_listed() {
    let backend = base_backend(); // models: ["d4f"]
    assert!(backend.validate(Some("d4f")).is_ok());
}

#[test]
fn inference_backend_validate_no_lockout_is_skipped_when_models_list_is_empty() {
    let mut backend = base_backend();
    backend.models = Vec::new();
    assert!(backend.validate(Some("anything")).is_ok());
}

#[test]
fn inference_backend_validate_no_lockout_is_skipped_without_a_current_model() {
    let backend = base_backend(); // models: ["d4f"]
    assert!(backend.validate(None).is_ok());
}

#[test]
fn inference_backend_validation_reports_every_violation() {
    let mut backend = base_backend();
    backend.endpoint = "  ".to_string();
    backend.max_concurrent = 0;
    backend.max_queue_depth = -1;

    let violations = backend.validation_violations(None);
    assert_eq!(violations.len(), 3, "{violations:?}");
    assert!(violations
        .iter()
        .any(|error| error.contains("endpoint must not be empty")));
    assert!(violations
        .iter()
        .any(|error| error.contains("max_concurrent must be positive")));
    assert!(violations
        .iter()
        .any(|error| error.contains("max_queue_depth must be positive")));
}

#[test]
fn inference_backend_validation_honors_redacted_api_key_presence() {
    let mut backend = base_backend();
    backend.api_key = None;
    backend.api_key_env_var = Some("BACKEND_API_KEY".to_string());

    assert!(backend.validate(None).is_ok());
    let error = backend
        .validate_with_api_key_presence(None, true)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("must not set both api_key and api_key_env_var"),
        "{error}"
    );
}
