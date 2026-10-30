use super::*;
use crate::inference::admission::BackendAdmissionConfig;
use crate::inference::backends::provider::BackendProviderKind;
use crate::lean_vocab_test::lean_backend_health_admission_cases;
use crate::OpenAiWireApi;

fn base_backend() -> InferenceBackend {
    serde_json::from_value(serde_json::json!({
        "node_did": "did:key:backend-owner",
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
    value["catalog_freshness"] = serde_json::json!({"version":1,"entries":[]});
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
    for field in ["_docID", "probe_status", "catalogs", "catalog_freshness"] {
        assert!(config.get(field).is_none());
    }
}

#[test]
fn inference_backend_from_value_requires_provider_kind_and_explicit_auth() {
    for field in ["provider_kind", "auth", "node_did"] {
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
fn node_oauth_has_one_canonical_serde_tag() {
    assert_eq!(
        serde_json::to_value(BackendAuth::NodeOAuth { account_ref: None }).unwrap(),
        serde_json::json!({"kind": "node_oauth"})
    );
    assert!(
        serde_json::from_value::<BackendAuth>(serde_json::json!({"kind": "node_o_auth"})).is_err(),
        "the retired acronym-splitting spelling must not become a compatibility alias"
    );
}

#[test]
fn node_oauth_account_ref_round_trips_and_stays_absent_when_none() {
    for text in [
        r#"{"kind":"node_oauth","account_ref":"acct-1"}"#,
        r#"{"kind":"node_oauth"}"#,
    ] {
        let auth: BackendAuth = serde_json::from_str(text).unwrap();
        assert_eq!(serde_json::to_string(&auth).unwrap(), text);
    }
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
            crate::inference::admission::document_configured_from_fields(
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
    assert!(BackendAuth::NodeOAuth { account_ref: None }
        .resolve_api_key()
        .is_err());
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
    let error = crate::inference::resolved::resolve_backend_api_key(&backend)
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
    backend.node_did = " ".into();
    backend.backend_id = " ".into();
    backend.endpoint = " ".into();
    backend.max_concurrent = Some(0);
    backend.max_queue_depth = Some(-1);
    let violations = backend.validation_violations();
    for field in [
        "node_did",
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
fn anthropic_api_key_takes_keys_on_the_fixed_endpoint_only() {
    let endpoint = crate::inference::clients::claude_subscription::ANTHROPIC_API_ENDPOINT;
    let mut backend = base_backend();
    backend.provider_kind = BackendProviderKind::AnthropicApiKey;
    backend.endpoint = endpoint.into();
    for auth in [
        BackendAuth::ApiKey {
            key: "placeholder-key".into(),
        },
        BackendAuth::Environment {
            variable: "ANTHROPIC_API_KEY".into(),
        },
        BackendAuth::Unauthenticated,
    ] {
        backend.auth = auth;
        backend.validate().unwrap();
    }
    backend.auth = BackendAuth::NodeOAuth { account_ref: None };
    assert!(backend.validate().is_err());

    backend.auth = BackendAuth::Environment {
        variable: "ANTHROPIC_API_KEY".into(),
    };
    backend.endpoint = format!("{endpoint}/");
    backend.validate().unwrap();
    backend.endpoint = "https://proxy.example.invalid/v1".into();
    let error = backend.validate().unwrap_err().to_string();
    assert!(error.contains(endpoint), "{error}");
}

#[test]
fn inference_backend_validation_requires_provider_compatible_auth() {
    let mut backend = base_backend();
    backend.auth = BackendAuth::NodeOAuth { account_ref: None };
    assert!(backend.validate().is_err());
    for provider in [
        BackendProviderKind::ChatGptCodex,
        BackendProviderKind::XaiGrokOAuth,
        BackendProviderKind::ClaudeCliSubscription,
    ] {
        backend.provider_kind = provider;
        backend.auth = BackendAuth::NodeOAuth { account_ref: None };
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
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await?);
    crate::ensure_runtime_schemas(&node).await?;
    crate::ensure_node(&node, &base_backend().node_did).await?;
    let access = crate::config_client::ConfigAccess::local(node.clone());
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
        {"node_did":null,"observed_at":"2026-01-01T00:00:00Z","models":[{"model_name":"shared","display_name":null,"context_window":null,"max_context_window":null,"max_output_tokens":null,"reasoning_efforts":null}]},
        {"node_did":"did:key:invoker","observed_at":"2026-01-02T00:00:00Z","models":[{"model_name":"private","display_name":null,"context_window":null,"max_context_window":null,"max_output_tokens":null,"reasoning_efforts":null}]}
    ]);
    let observation = serde_json::json!({"catalogs":{"entries":catalogs.clone()},"probe_status":"healthy","last_probe":"2026-01-02T00:00:00Z"});
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
    other.node_did = "did:key:other-owner".into();
    other.backend_id = backend.backend_id.clone();
    other.endpoint = "http://other.example/v1".into();
    crate::ensure_node(&node, &other.node_did).await?;
    crate::config_client::write_inference_backend_document(&access, &other).await?;
    let mut replacement = base_backend();
    replacement.backend_id = backend.backend_id.clone();
    let replaced_id =
        crate::config_client::write_inference_backend_document(&access, &replacement).await?;
    assert_eq!(replaced_id, doc_id);
    let loaded = lookup_backend(&node, &backend.node_did, &backend.backend_id)
        .await?
        .unwrap();
    assert_eq!(
        serde_json::to_value(&loaded)?,
        serde_json::to_value(&replacement)?
    );
    assert_eq!(loaded.effective_max_concurrent(), 1);
    assert_eq!(loaded.effective_max_queue_depth(), 100);
    let observed = lookup_backend_observation(&node, &backend.node_did, &backend.backend_id)
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
    let other_loaded = lookup_backend(&node, &other.node_did, &other.backend_id)
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
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await?);
    crate::ensure_runtime_schemas(&node).await?;
    crate::ensure_node(&node, &base_backend().node_did).await?;
    let access = crate::config_client::ConfigAccess::local(node.clone());
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
    let loaded = lookup_backend(&node, &backend.node_did, &backend.backend_id)
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
async fn backend_lists_skip_an_unknown_provider_kind_and_lookups_stay_strict() -> Result<()> {
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await?);
    crate::ensure_runtime_schemas(&node).await?;
    let owner = base_backend().node_did;
    crate::ensure_node(&node, &owner).await?;
    let access = crate::config_client::ConfigAccess::local(node.clone());
    crate::config_client::write_inference_backend_document(&access, &base_backend()).await?;
    let mut future = serde_json::to_value(base_backend())?;
    future["backend_id"] = "future".into();
    future["provider_kind"] = "FutureProviderKind".into();
    future["endpoint"] = "https://example.invalid/v1".into();
    future["enabled"] = true.into();
    access
        .write(
            "test.backend.future_kind",
            &format!(
                "mutation {{ create_InferenceBackend(input: {}) {{ _docID }} }}",
                gents_protocol::graphql::graphql_input_literal(&future)?
            ),
        )
        .await?;

    let ids = |backends: Vec<InferenceBackend>| {
        backends
            .into_iter()
            .map(|backend| backend.backend_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(list_all_backends(&node).await?), ["reviewers"]);
    assert_eq!(ids(list_enabled_backends(&node).await?), ["reviewers"]);
    assert_eq!(
        ids(list_enabled_backends_for_agent(&node, &owner).await?),
        ["reviewers"]
    );
    let error = lookup_backend(&node, &owner, "future")
        .await
        .expect_err("a single lookup stays strict");
    assert!(
        format!("{error:#}").contains("FutureProviderKind"),
        "{error:#}"
    );
    Ok(())
}

#[tokio::test]
async fn discovery_rejects_wrong_scope_and_stale_connection_without_losing_catalog() -> Result<()> {
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await?);
    crate::ensure_runtime_schemas(&node).await?;
    crate::ensure_node(&node, &base_backend().node_did).await?;
    let access = crate::config_client::ConfigAccess::local(node.clone());
    let backend = base_backend();
    crate::config_client::write_inference_backend_document(&access, &backend).await?;
    let catalog = BackendModelCatalog {
        node_did: None,
        observed_at: "2026-09-09T10:00:00Z".into(),
        models: vec![crate::documents::AdvertisedModel {
            model_name: "advertised-model".into(),
            display_name: None,
            context_window: Some(524288),
            max_context_window: None,
            max_output_tokens: None,
            reasoning_efforts: None,
        }],
    };
    record_model_catalog(&node, &backend, catalog.clone()).await?;
    let mut wrong_scope = catalog.clone();
    wrong_scope.node_did = Some("did:key:other".into());
    assert!(record_model_catalog(&node, &backend, wrong_scope)
        .await
        .is_err());
    let mut older = catalog.clone();
    older.observed_at = "2026-09-09T09:00:00Z".into();
    older.models.clear();
    record_model_catalog(&node, &backend, older).await?;
    let mut rediscovered = catalog.clone();
    rediscovered.observed_at = "2026-09-09T11:00:00Z".into();
    record_model_catalog(&node, &backend, rediscovered.clone()).await?;
    assert_eq!(
        lookup_backend_observation(&node, &backend.node_did, &backend.backend_id)
            .await?
            .unwrap()
            .catalogs,
        vec![rediscovered.clone()],
        "the public observation must retain the latest successful discovery"
    );
    let mut changed = backend.clone();
    changed.endpoint = "http://127.0.0.1:9000/v1".into();
    crate::config_client::write_inference_backend_document(&access, &changed).await?;
    assert!(record_model_catalog(&node, &backend, catalog.clone())
        .await
        .is_err());
    assert_eq!(
        lookup_backend_observation(&node, &backend.node_did, &backend.backend_id)
            .await?
            .unwrap()
            .catalogs,
        vec![rediscovered]
    );
    Ok(())
}

#[tokio::test]
async fn catalog_transaction_preserves_explicit_empty_lists_and_rollback() -> Result<()> {
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await?);
    crate::ensure_runtime_schemas(&node).await?;
    crate::ensure_node(&node, &base_backend().node_did).await?;
    let access = crate::config_client::ConfigAccess::local(node.clone());
    let backend = base_backend();
    crate::config_client::write_inference_backend_document(&access, &backend).await?;
    let catalog = BackendModelCatalog {
        node_did: None,
        observed_at: "2026-09-09T10:00:00Z".into(),
        models: vec![crate::documents::AdvertisedModel {
            model_name: "exact-model".into(),
            display_name: None,
            context_window: None,
            max_context_window: None,
            max_output_tokens: None,
            reasoning_efforts: Some(Vec::new()),
        }],
    };
    access
        .transact_write("test.catalog.transaction", |txn| {
            let catalog = catalog.clone();
            let backend = &backend;
            Box::pin(async move { record_model_catalog_in_txn(txn, backend, catalog).await })
        })
        .await?;
    assert_eq!(
        lookup_backend_observation(&node, &backend.node_did, &backend.backend_id)
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
        .transact_write("test.catalog.rollback", |txn| {
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
        lookup_backend_observation(&node, &backend.node_did, &backend.backend_id)
            .await?
            .unwrap()
            .catalogs,
        vec![catalog]
    );
    record_model_catalog(&node, &backend, empty.clone()).await?;
    assert_eq!(
        lookup_backend_observation(&node, &backend.node_did, &backend.backend_id)
            .await?
            .unwrap()
            .catalogs,
        vec![empty]
    );
    node.shutdown().await;
    Ok(())
}

async fn spawn_echoing_models_server(secret: &'static str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0; 8192];
        let count = stream.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..count]).to_ascii_lowercase();
        assert!(request.contains(&format!("authorization: bearer {secret}")));
        let body = serde_json::json!({"data": [{
            "id": "claude-opus-5-5",
            "display_name": "Claude Opus 5.5",
            "max_input_tokens": 1_000_000,
            "max_tokens": 128_000,
            "capabilities": {"effort": {"supported": true, "low": {"supported": true}, "high": {"supported": true}}},
            "access_token": secret,
            "authorization": format!("Bearer {secret}"),
        }], "has_more": false})
        .to_string();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    format!("http://{address}/v1")
}

#[tokio::test]
async fn operator_discovery_publishes_scoped_credential_free_catalog() -> Result<()> {
    const SECRET: &str = "subscription-access-token-never-stored";
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await?);
    crate::ensure_runtime_schemas(&node).await?;
    let owner = base_backend().node_did;
    crate::ensure_node(&node, &owner).await?;
    let access = crate::config_client::ConfigAccess::local(node.clone());
    let endpoint = spawn_echoing_models_server(SECRET).await;
    let mut claude = base_backend();
    claude.backend_id = "claude".into();
    claude.provider_kind = BackendProviderKind::ClaudeCliSubscription;
    claude.auth = BackendAuth::NodeOAuth { account_ref: None };
    claude.endpoint = endpoint.clone();
    let mut elsewhere = claude.clone();
    elsewhere.backend_id = "claude-elsewhere".into();
    elsewhere.endpoint = "http://127.0.0.1:1/v1".into();
    for backend in [&claude, &elsewhere] {
        crate::config_client::write_inference_backend_document(&access, backend).await?;
    }
    let credential = crate::inference::auth::credential::OAuthCredential {
        doc_id: None,
        credential_id: format!("claude-subscription:{owner}"),
        node_did: owner.clone(),
        provider: crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER.into(),
        access_token: SECRET.into(),
        refresh_token: format!("{SECRET}-refresh"),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        last_refresh: None,
        enabled: true,
        account_ref: None,
        connected_at: None,
        provider_account_key: None,
        label: None,
    };
    let models = crate::inference::backends::provider::discover_models(
        &reqwest::Client::new(),
        claude.provider_kind,
        &endpoint,
        None,
        Some(&credential),
    )
    .await?;

    let recorded = record_connection_catalog_on(
        &access,
        &owner,
        claude.provider_kind,
        &endpoint,
        &BackendAuth::NodeOAuth { account_ref: None },
        models,
    )
    .await?;
    assert_eq!(recorded, 1, "only the backend on this exact connection");

    let observation = lookup_backend_observation(&node, &owner, "claude")
        .await?
        .unwrap();
    let catalog = observation.catalog_for(Some(&owner))?.expect("OAuth scope");
    assert_eq!(catalog.models[0].model_name, "claude-opus-5-5");
    assert_eq!(catalog.models[0].context_window, Some(1_000_000));
    assert_eq!(
        catalog.models[0].reasoning_efforts.as_deref(),
        Some(
            [
                crate::inference::resolved::ReasoningEffort::Low,
                crate::inference::resolved::ReasoningEffort::High
            ]
            .as_slice()
        )
    );
    assert!(observation.catalog_for(None)?.is_none());
    assert!(
        lookup_backend_observation(&node, &owner, "claude-elsewhere")
            .await?
            .unwrap()
            .catalogs
            .is_empty()
    );

    let stored = node.execute("{ InferenceBackend { catalogs } }").await;
    assert!(!stored.has_errors(), "{:?}", stored.errors);
    let stored = serde_json::to_string(&stored.data)?;
    assert!(stored.contains("claude-opus-5-5"));
    assert!(
        !stored.contains(SECRET),
        "credential material persisted: {stored}"
    );
    node.shutdown().await;
    Ok(())
}

async fn stored_catalog_row(
    access: &crate::config_client::ConfigAccess,
    backend: &InferenceBackend,
) -> Result<serde_json::Value> {
    let filter = scope_filter(&backend.node_did, &backend.backend_id)?;
    let response = access.execute(&format!(
        "{{ InferenceBackend(filter: {filter}) {{ _docID backend_id catalogs catalog_freshness }} }}"
    )).await?;
    Ok(response["data"]["InferenceBackend"][0].clone())
}

fn catalog_action(
    action: &serde_json::Value,
    backend: &InferenceBackend,
) -> Result<BackendModelCatalog> {
    use chrono::TimeZone as _;
    let payload = action["payload"].as_str().unwrap();
    let models = serde_json::from_str::<Vec<crate::documents::AdvertisedModel>>(payload)?;
    assert_eq!(
        serde_json::to_string(&models)?,
        payload,
        "model inputs must use canonical native serialization"
    );
    Ok(BackendModelCatalog {
        node_did: backend.catalog_scope().map(str::to_owned),
        observed_at: chrono::Utc
            .timestamp_opt(action["observed_at"].as_i64().unwrap(), 0)
            .single()
            .unwrap()
            .to_rfc3339(),
        models,
    })
}

#[tokio::test]
async fn generated_catalog_refreshes_preserve_payload_commits_and_latest_freshness() -> Result<()> {
    let node = std::sync::Arc::new(crate::test_support::node_builder().build().await?);
    crate::ensure_runtime_schemas(&node).await?;
    crate::ensure_node(&node, &base_backend().node_did).await?;
    let access = crate::config_client::ConfigAccess::local(node.clone());
    for (index, case) in crate::lean_vocab_test::lean_catalog_refresh_cases()
        .iter()
        .enumerate()
    {
        let mut backend = base_backend();
        backend.backend_id = format!("catalog-case-{index}");
        crate::config_client::write_inference_backend_document(&access, &backend).await?;
        let row = stored_catalog_row(&access, &backend).await?;
        let doc_id = row["_docID"].as_str().unwrap();
        let fields = ["catalogs", "catalog_freshness"];
        let mut previous = Vec::new();
        for field in fields {
            previous.push(
                crate::inference::rendered_request::commits::field_commit(&access, doc_id, field)
                    .await?,
            );
        }
        let mut writes = [0_u64; 2];
        for action in case["actions"].as_array().unwrap() {
            record_model_catalog(&node, &backend, catalog_action(action, &backend)?).await?;
            for (index, field) in fields.iter().enumerate() {
                let current = crate::inference::rendered_request::commits::field_commit(
                    &access, doc_id, field,
                )
                .await?;
                writes[index] += u64::from(current != previous[index]);
                previous[index] = current;
            }
        }
        assert_eq!(
            writes,
            [
                case["content_writes"].as_u64().unwrap(),
                case["watermark_writes"].as_u64().unwrap()
            ],
            "{}",
            case["witness"]
        );
        let row = stored_catalog_row(&access, &backend).await?;
        let raw: InferenceBackendObservation = serde_json::from_value(row.clone())?;
        let projected = lookup_backend_observation(&node, &backend.node_did, &backend.backend_id)
            .await?
            .unwrap();
        if case["expected"].is_null() {
            assert!(raw.catalogs.is_empty());
            assert!(projected.catalogs.is_empty());
        } else {
            let expected = &case["expected"];
            assert_eq!(
                serde_json::to_string(&raw.catalogs[0].models)?,
                expected["payload"].as_str().unwrap(),
                "{}",
                case["witness"]
            );
            assert_eq!(
                chrono::DateTime::parse_from_rfc3339(&raw.catalogs[0].observed_at)?.timestamp(),
                expected["content_at"].as_i64().unwrap()
            );
            assert_eq!(projected.catalogs[0].models, raw.catalogs[0].models);
            assert_eq!(
                chrono::DateTime::parse_from_rfc3339(&projected.catalogs[0].observed_at)?
                    .timestamp(),
                expected["observed_at"].as_i64().unwrap()
            );
        }
    }
    node.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn catalog_freshness_survives_reopen_and_adopts_retained_payloads() -> Result<()> {
    use crate::NodeIdentity as _;
    let directory = tempfile::tempdir()?;
    let identity = crate::KeyIdentity::load_or_create(directory.path().join("node.key"), None)?;
    let open = || {
        crate::defra_node::EmbeddedNode::builder()
            .data_path(directory.path())
            .with_node_identity_did(identity.did())
            .build()
    };
    let node = std::sync::Arc::new(open().await?);
    crate::ensure_runtime_schemas(&node).await?;
    let backend = base_backend();
    crate::ensure_node(&node, &backend.node_did).await?;
    let access = crate::config_client::ConfigAccess::local(node.clone());
    crate::config_client::write_inference_backend_document(&access, &backend).await?;
    let case = crate::lean_vocab_test::lean_catalog_refresh_cases()
        .iter()
        .find(|case| case["witness"] == "catalog.delayed-change-after-identical")
        .unwrap();
    let actions = case["actions"].as_array().unwrap();
    let original = catalog_action(&actions[0], &backend)?;
    access.write("test.catalog.retained", &format!(
        "mutation {{ update_InferenceBackend(filter: {}, input: {{ catalogs: {} }}) {{ _docID }} }}",
        scope_filter(&backend.node_did, &backend.backend_id)?,
        gents_protocol::graphql::graphql_input_literal(&serde_json::json!({"entries":[original.clone()]}))?
    )).await?;
    let row = stored_catalog_row(&access, &backend).await?;
    assert!(row["catalog_freshness"].is_null());
    assert_eq!(
        decode_backend_observation(row)?.catalogs,
        std::slice::from_ref(&original)
    );
    record_model_catalog(&node, &backend, original.clone()).await?;
    record_model_catalog(&node, &backend, catalog_action(&actions[1], &backend)?).await?;
    let stored = stored_catalog_row(&access, &backend).await?;
    assert_eq!(
        serde_json::from_value::<InferenceBackendObservation>(stored.clone())?.catalogs,
        [original]
    );
    drop(access);
    node.shutdown().await;
    drop(node);
    let node = std::sync::Arc::new(open().await?);
    record_model_catalog(&node, &backend, catalog_action(&actions[2], &backend)?).await?;
    let access = crate::config_client::ConfigAccess::local(node.clone());
    assert_eq!(
        stored_catalog_row(&access, &backend).await?,
        stored,
        "a delayed result after reopen must not write either payload or freshness"
    );
    let observed = lookup_backend_observation(&node, &backend.node_did, &backend.backend_id)
        .await?
        .unwrap();
    assert_eq!(
        serde_json::to_string(&observed.catalogs[0].models)?,
        case["expected"]["payload"].as_str().unwrap()
    );
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(&observed.catalogs[0].observed_at)?.timestamp(),
        case["expected"]["observed_at"].as_i64().unwrap()
    );
    node.shutdown().await;
    Ok(())
}
