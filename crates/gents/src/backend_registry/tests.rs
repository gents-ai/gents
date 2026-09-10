use super::*;
use crate::admission::BackendAdmissionConfig;
use crate::backend_provider::BackendProviderKind;
use crate::lean_vocab_test::lean_backend_health_admission_cases;
use crate::OpenAiWireApi;

fn base_backend() -> InferenceBackend {
    serde_json::from_value(serde_json::json!({
        "agent_did": "did:key:backend-owner",
        "backend_id": "reviewers",
        "name": "Reviewers",
        "provider_kind": "OpenAiCompatible",
        "endpoint": "http://127.0.0.1:8000/v1",
        "auth": {"kind": "unauthenticated"}
    }))
    .unwrap()
}

#[test]
fn inference_backend_from_value_parses() {
    let mut value = serde_json::to_value(base_backend()).unwrap();
    value["auth"] = serde_json::json!({"kind": "api_key", "key": "raw-key"});
    value["openai_wire_api"] = "chat_completions".into();
    value["max_concurrent"] = 4.into();
    value["max_queue_depth"] = 9.into();
    value["_docID"] = "physical-backend".into();
    value["probe_status"] = "healthy".into();
    value["catalogs"] = serde_json::json!([]);
    let backend = InferenceBackend::from_value(&value).unwrap();
    assert_eq!(
        backend.auth,
        BackendAuth::ApiKey {
            key: "raw-key".into()
        }
    );
    assert_eq!(
        backend.openai_wire_api,
        Some(OpenAiWireApi::ChatCompletions)
    );
    assert_eq!(backend.max_concurrent, Some(4));
    assert_eq!(backend.max_queue_depth, Some(9));
    let config = serde_json::to_value(backend).unwrap();
    for field in ["_docID", "probe_status", "catalogs"] {
        assert!(config.get(field).is_none());
    }
}

#[test]
fn inference_backend_from_value_requires_provider_kind_and_explicit_auth() {
    for field in ["provider_kind", "auth", "agent_did"] {
        let mut value = serde_json::to_value(base_backend()).unwrap();
        value.as_object_mut().unwrap().remove(field);
        let error = InferenceBackend::from_value(&value).unwrap_err();
        assert!(format!("{error:#}").contains(field), "{error:#}");
    }
    let mut value = serde_json::to_value(base_backend()).unwrap();
    value["auth"] = serde_json::json!({"kind":"api_key", "key":"key", "variable":"KEY"});
    assert!(
        InferenceBackend::from_value(&value).is_err(),
        "competing credential selection must fail"
    );
}

#[test]
fn principal_oauth_has_one_canonical_serde_tag() {
    assert_eq!(
        serde_json::to_value(BackendAuth::PrincipalOAuth).unwrap(),
        serde_json::json!({"kind": "principal_oauth"})
    );
    assert!(
        serde_json::from_value::<BackendAuth>(serde_json::json!({"kind": "principal_o_auth"}))
            .is_err(),
        "the retired acronym-splitting spelling must not become a compatibility alias"
    );
}

#[test]
fn generated_backend_health_admission_cases_match_registry_and_admission_policy() {
    let cases = lean_backend_health_admission_cases();
    assert_eq!(cases.len(), 7);

    for case in cases {
        let mut backend = base_backend();
        backend.backend_id = case.name.clone();
        backend.enabled = case.enabled;
        let observation = InferenceBackendObservation {
            backend_id: backend.backend_id.clone(),
            catalogs: Vec::new(),
            probe_status: Some(case.probe_status.clone()),
            last_probe: None,
        };
        let admission_config = BackendAdmissionConfig::from_backend(&backend, &observation)
            .expect("valid backend config and observation");

        assert_eq!(
            crate::admission::document_configured_from_fields(
                backend.enabled,
                observation.probe_status.as_deref().unwrap()
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
/// witness through `derive_display_state` and compares availability with Lean.
/// An explicit input table checks the display labels, including unknown status.
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

    for (enabled, status, expected) in [
        (false, "healthy", "disabled"),
        (true, "healthy", "available"),
        (true, "unhealthy", "unhealthy"),
        (true, "stale", "stale"),
        (true, "rate_limited", "rate-limited"),
        (true, "circuit_open", "circuit-open"),
        (true, "unknown", "unknown"),
        (true, "unrecognized", "unknown"),
    ] {
        assert_eq!(derive_display_state(enabled, status), expected);
    }
    for case in cases {
        assert_eq!(
            derive_display_state(case.enabled, &case.probe_status) == "available",
            case.expected_available,
            "{}",
            case.name
        );
    }
}

#[test]
fn resolve_backend_api_key_uses_explicit_credentials() {
    assert_eq!(
        BackendAuth::ApiKey {
            key: "raw-key".into()
        }
        .resolve_api_key()
        .unwrap()
        .as_deref(),
        Some("raw-key")
    );
    assert_eq!(
        BackendAuth::Unauthenticated.resolve_api_key().unwrap(),
        None
    );
    assert!(BackendAuth::PrincipalOAuth.resolve_api_key().is_err());
    assert!(BackendAuth::ApiKey { key: " ".into() }
        .resolve_api_key()
        .is_err());
}

#[test]
fn resolve_backend_api_key_reads_selected_environment() {
    let variable = "GENTS_BACKEND_REGISTRY_EXPLICIT_KEY";
    std::env::set_var(variable, "env-key");
    let auth = BackendAuth::Environment {
        variable: variable.into(),
    };
    assert_eq!(auth.resolve_api_key().unwrap().as_deref(), Some("env-key"));
    std::env::set_var(variable, " ");
    assert!(
        auth.resolve_api_key().is_err(),
        "blank environment is not unauthenticated"
    );
    std::env::remove_var(variable);
    assert!(
        auth.resolve_api_key().is_err(),
        "missing environment is not unauthenticated"
    );
}

#[test]
fn resolve_backend_api_key_errors_when_env_var_named_but_unset() {
    let mut backend = base_backend();
    backend.auth = BackendAuth::Environment {
        variable: "BACKEND_REGISTRY_TEST_KEY_MISSING_1338".into(),
    };
    let error = crate::config::resolve_backend_api_key(&backend)
        .expect_err("missing selected credential must fail");
    let message = format!("{error:#}");
    assert!(message.contains(&backend.backend_id), "{message}");
    assert!(
        message.contains("BACKEND_REGISTRY_TEST_KEY_MISSING_1338"),
        "{message}"
    );
}

#[test]
fn inference_backend_validation_preserves_defaults_and_rejects_invalid_values() {
    let backend = base_backend();
    backend.validate().unwrap();
    assert_eq!(backend.effective_max_concurrent(), 1);
    assert_eq!(backend.effective_max_queue_depth(), 100);
    let mut backend = backend;
    backend.max_queue_depth = Some(0);
    backend
        .validate()
        .expect("zero explicitly disables queueing");
    for value in [0, -1] {
        backend.max_concurrent = Some(value);
        assert!(backend.validate().is_err());
    }
    backend.max_concurrent = None;
    backend.max_queue_depth = Some(-1);
    assert!(backend.validate().is_err());
    backend.max_queue_depth = None;
    for value in [0, -1] {
        backend.connect_timeout_secs = Some(value);
        assert!(backend.validate().is_err());
        backend.connect_timeout_secs = None;
        backend.discovery_timeout_secs = Some(value);
        assert!(backend.validate().is_err());
        backend.discovery_timeout_secs = None;
    }
}

#[test]
fn inference_backend_validation_reports_every_violation() {
    let mut backend = base_backend();
    backend.agent_did = " ".into();
    backend.backend_id = " ".into();
    backend.endpoint = " ".into();
    backend.max_concurrent = Some(0);
    backend.max_queue_depth = Some(-1);
    let violations = backend.validation_violations();
    for field in [
        "agent_did",
        "backend_id",
        "endpoint",
        "max_concurrent",
        "max_queue_depth",
    ] {
        assert!(
            violations.iter().any(|error| error.contains(field)),
            "{violations:?}"
        );
    }
}

#[test]
fn inference_backend_validation_requires_provider_compatible_auth() {
    let mut backend = base_backend();
    backend.auth = BackendAuth::PrincipalOAuth;
    assert!(backend.validate().is_err());
    for provider in [
        BackendProviderKind::ChatGptCodex,
        BackendProviderKind::XaiGrokOAuth,
        BackendProviderKind::ClaudeCliSubscription,
    ] {
        backend.provider_kind = provider;
        backend.auth = BackendAuth::PrincipalOAuth;
        backend.validate().unwrap();
        for auth in [
            BackendAuth::Unauthenticated,
            BackendAuth::ApiKey { key: "key".into() },
            BackendAuth::Environment {
                variable: "KEY".into(),
            },
        ] {
            backend.auth = auth;
            assert!(backend.validate().is_err());
        }
    }
}

#[test]
fn admission_rejects_unrelated_observation_and_defaults_capacity() {
    let backend = base_backend();
    let mut observation = InferenceBackendObservation {
        backend_id: "different-backend".into(),
        catalogs: Vec::new(),
        probe_status: Some("healthy".into()),
        last_probe: None,
    };
    assert!(BackendAdmissionConfig::from_backend(&backend, &observation).is_err());
    observation.backend_id = backend.backend_id.clone();
    let admission = BackendAdmissionConfig::from_backend(&backend, &observation).unwrap();
    assert_eq!(
        (admission.max_concurrent, admission.max_queue_depth),
        (1, 100)
    );
    assert!(admission.is_available());
    observation.probe_status = None;
    assert!(
        !BackendAdmissionConfig::from_backend(&backend, &observation)
            .unwrap()
            .is_available()
    );
}

#[tokio::test]
async fn scoped_backend_replacement_preserves_observations_and_resets_defaults() -> Result<()> {
    let node = std::sync::Arc::new(EmbeddedNode::builder().build().await?);
    crate::ensure_runtime_schemas(&node).await?;
    crate::ensure_agent_principal(&node, &base_backend().agent_did).await?;
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    let mut backend = base_backend();
    backend.backend_id = "reviewers\"quoted".into();
    backend.max_concurrent = Some(4);
    backend.max_queue_depth = Some(0);
    backend.connect_timeout_secs = Some(3);
    backend.discovery_timeout_secs = Some(9);
    backend.enabled = false;
    backend.tags = vec!["old".into()];
    let doc_id = crate::config_client::write_inference_backend_document(&access, &backend).await?;
    let catalogs = serde_json::json!([
        {"agent_did":null,"observed_at":"2026-01-01T00:00:00Z","models":[{"model_name":"shared","display_name":null,"context_window":null,"max_output_tokens":null,"reasoning_efforts":null}]},
        {"agent_did":"did:key:invoker","observed_at":"2026-01-02T00:00:00Z","models":[{"model_name":"private","display_name":null,"context_window":null,"max_output_tokens":null,"reasoning_efforts":null}]}
    ]);
    let observation = serde_json::json!({"catalogs":catalogs,"probe_status":"healthy","last_probe":"2026-01-02T00:00:00Z"});
    access
        .write(
            "test.backend.observation",
            &format!(
                r#"mutation {{ update_InferenceBackend(docID: "{}", input: {}) {{ _docID }} }}"#,
                escape_graphql_string(&doc_id),
                gents_protocol::graphql::graphql_input_literal(&observation)?
            ),
        )
        .await?;
    let mut other = base_backend();
    other.agent_did = "did:key:other-owner".into();
    other.backend_id = backend.backend_id.clone();
    other.endpoint = "http://other.example/v1".into();
    crate::ensure_agent_principal(&node, &other.agent_did).await?;
    crate::config_client::write_inference_backend_document(&access, &other).await?;
    let mut replacement = base_backend();
    replacement.backend_id = backend.backend_id.clone();
    let replaced_id =
        crate::config_client::write_inference_backend_document(&access, &replacement).await?;
    assert_eq!(replaced_id, doc_id);
    let loaded = lookup_backend(&node, &backend.agent_did, &backend.backend_id)
        .await?
        .unwrap();
    assert_eq!(
        serde_json::to_value(&loaded)?,
        serde_json::to_value(&replacement)?
    );
    assert_eq!(loaded.effective_max_concurrent(), 1);
    assert_eq!(loaded.effective_max_queue_depth(), 100);
    let observed = lookup_backend_observation(&node, &backend.agent_did, &backend.backend_id)
        .await?
        .unwrap();
    assert_eq!(serde_json::to_value(&observed.catalogs)?, catalogs);
    assert_eq!(observed.probe_status.as_deref(), Some("healthy"));
    assert_eq!(observed.last_probe.as_deref(), Some("2026-01-02T00:00:00Z"));
    assert_eq!(
        observed.catalog_for(None)?.unwrap().models[0].model_name,
        "shared"
    );
    assert_eq!(
        observed
            .catalog_for(Some("did:key:invoker"))?
            .unwrap()
            .models[0]
            .model_name,
        "private"
    );
    assert!(observed.catalog_for(Some("did:key:stranger"))?.is_none());
    let other_loaded = lookup_backend(&node, &other.agent_did, &other.backend_id)
        .await?
        .unwrap();
    assert_eq!(other_loaded.endpoint, other.endpoint);
    assert!(
        lookup_backend(&node, "did:key:missing", &backend.backend_id)
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn duplicate_backend_owner_keys_fail_without_overwriting_documents() -> Result<()> {
    let node = std::sync::Arc::new(EmbeddedNode::builder().build().await?);
    crate::ensure_runtime_schemas(&node).await?;
    crate::ensure_agent_principal(&node, &base_backend().agent_did).await?;
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    let backend = base_backend();
    crate::config_client::write_inference_backend_document(&access, &backend).await?;
    let mut duplicate = backend.clone();
    duplicate.name = "A physically distinct duplicate".into();
    let duplicate_result = access
        .write(
            "test.backend.duplicate",
            &format!(
                "mutation {{ create_InferenceBackend(input: {}) {{ _docID }} }}",
                gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(&duplicate)?)?
            ),
        )
        .await;
    assert!(
        duplicate_result.is_err(),
        "the canonical unique owner/ID index must reject duplicates"
    );
    let loaded = lookup_backend(&node, &backend.agent_did, &backend.backend_id)
        .await?
        .expect("original backend remains");
    assert_eq!(loaded.name, backend.name);
    let records = list_all_backends(&node).await?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].name, backend.name);
    let response = node.execute("{ InferenceBackend { probe_status } }").await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.unwrap();
    for row in data["InferenceBackend"].as_array().unwrap() {
        assert!(row["probe_status"].is_null());
    }
    Ok(())
}

#[tokio::test]
async fn discovery_rejects_wrong_scope_and_stale_connection_without_losing_catalog() -> Result<()> {
    let node = std::sync::Arc::new(EmbeddedNode::builder().build().await?);
    crate::ensure_runtime_schemas(&node).await?;
    crate::ensure_agent_principal(&node, &base_backend().agent_did).await?;
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    let backend = base_backend();
    crate::config_client::write_inference_backend_document(&access, &backend).await?;
    let catalog = BackendModelCatalog {
        agent_did: None,
        observed_at: "2026-09-09T10:00:00Z".into(),
        models: vec![crate::document_config::AdvertisedModel {
            model_name: "advertised-model".into(),
            display_name: None,
            context_window: Some(524288),
            max_output_tokens: None,
            reasoning_efforts: None,
        }],
    };
    record_model_catalog(&node, &backend, catalog.clone()).await?;
    let mut wrong_scope = catalog.clone();
    wrong_scope.agent_did = Some("did:key:other".into());
    assert!(record_model_catalog(&node, &backend, wrong_scope)
        .await
        .is_err());
    let mut older = catalog.clone();
    older.observed_at = "2026-09-09T09:00:00Z".into();
    older.models.clear();
    record_model_catalog(&node, &backend, older).await?;
    assert_eq!(
        lookup_backend_observation(&node, &backend.agent_did, &backend.backend_id)
            .await?
            .unwrap()
            .catalogs,
        vec![catalog.clone()]
    );
    let mut changed = backend.clone();
    changed.endpoint = "http://127.0.0.1:9000/v1".into();
    crate::config_client::write_inference_backend_document(&access, &changed).await?;
    assert!(record_model_catalog(&node, &backend, catalog.clone())
        .await
        .is_err());
    assert_eq!(
        lookup_backend_observation(&node, &backend.agent_did, &backend.backend_id)
            .await?
            .unwrap()
            .catalogs,
        vec![catalog]
    );
    Ok(())
}

#[tokio::test]
async fn catalog_transaction_preserves_explicit_empty_lists_and_rollback() -> Result<()> {
    let node = std::sync::Arc::new(EmbeddedNode::builder().build().await?);
    crate::ensure_runtime_schemas(&node).await?;
    crate::ensure_agent_principal(&node, &base_backend().agent_did).await?;
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    let backend = base_backend();
    crate::config_client::write_inference_backend_document(&access, &backend).await?;
    let catalog = BackendModelCatalog {
        agent_did: None,
        observed_at: "2026-09-09T10:00:00Z".into(),
        models: vec![crate::document_config::AdvertisedModel {
            model_name: "exact-model".into(),
            display_name: None,
            context_window: None,
            max_output_tokens: None,
            reasoning_efforts: Some(Vec::new()),
        }],
    };
    access
        .transact("test.catalog.transaction", |txn| {
            let catalog = catalog.clone();
            let backend = &backend;
            Box::pin(async move { record_model_catalog_in_txn(txn, backend, catalog).await })
        })
        .await?;
    assert_eq!(
        lookup_backend_observation(&node, &backend.agent_did, &backend.backend_id)
            .await?
            .unwrap()
            .catalogs,
        vec![catalog.clone()]
    );
    let empty = BackendModelCatalog {
        observed_at: "2026-09-09T11:00:00Z".into(),
        models: Vec::new(),
        ..catalog.clone()
    };
    let failed: Result<()> = access
        .transact("test.catalog.rollback", |txn| {
            let empty = empty.clone();
            let backend = &backend;
            Box::pin(async move {
                record_model_catalog_in_txn(txn, backend, empty).await?;
                anyhow::bail!("force rollback")
            })
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(
        lookup_backend_observation(&node, &backend.agent_did, &backend.backend_id)
            .await?
            .unwrap()
            .catalogs,
        vec![catalog]
    );
    record_model_catalog(&node, &backend, empty.clone()).await?;
    assert_eq!(
        lookup_backend_observation(&node, &backend.agent_did, &backend.backend_id)
            .await?
            .unwrap()
            .catalogs,
        vec![empty]
    );
    node.shutdown().await;
    Ok(())
}
