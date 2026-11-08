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
fn private_credential_rotation_changes_the_keyed_connection_identity() {
    let mut backend = base_backend();
    backend.auth = BackendAuth::Credential {
        id: "private-reference".into(),
        version: "rotation-one".into(),
    };
    let before =
        crate::inference::admission::backend_connection_fingerprint(&backend.backend_fields());
    backend.auth = BackendAuth::Credential {
        id: "private-reference".into(),
        version: "rotation-two".into(),
    };
    let after =
        crate::inference::admission::backend_connection_fingerprint(&backend.backend_fields());
    assert_ne!(before, after);
    assert!(!before.contains("private-reference") && !before.contains("rotation-one"));
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
            metadata_doc_id: None,
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
fn an_account_pool_is_valid_only_for_the_claude_subscription() {
    let mut backend = base_backend();
    for allow_overage in [false, true] {
        backend.auth = BackendAuth::NodeOAuthPool { allow_overage };
        backend.provider_kind = BackendProviderKind::ClaudeCliSubscription;
        backend.validate().unwrap();
        for provider in [
            BackendProviderKind::ChatGptCodex,
            BackendProviderKind::XaiGrokOAuth,
        ] {
            backend.provider_kind = provider;
            let error = backend.validate().unwrap_err().to_string();
            assert!(error.contains("account pools rotate Claude subscription accounts"));
        }
    }
    backend.provider_kind = BackendProviderKind::ClaudeCliSubscription;
    backend.auth = BackendAuth::NodeOAuthPool {
        allow_overage: false,
    };
    assert!(backend.auth.uses_node_oauth());
    assert_eq!(backend.auth.oauth_account_ref(), None);
    assert!(backend.auth.resolve_api_key().is_err());
}

#[test]
fn pool_auth_round_trips_and_defaults_overage_off() {
    let auth: BackendAuth = serde_json::from_str(r#"{"kind":"node_oauth_pool"}"#).unwrap();
    assert_eq!(
        auth,
        BackendAuth::NodeOAuthPool {
            allow_overage: false
        }
    );
    assert_eq!(
        serde_json::to_string(&auth).unwrap(),
        r#"{"kind":"node_oauth_pool","allow_overage":false}"#
    );
    assert!(
        serde_json::from_str::<BackendAuth>(r#"{"kind":"node_oauth_pool","account_ref":"a"}"#)
            .is_err()
    );
}

#[test]
fn admission_rejects_unrelated_observation_and_defaults_capacity() {
    let backend = base_backend();
    let mut observation = InferenceBackendObservation {
        metadata_doc_id: None,
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

async fn backend_for_user(
    access: &crate::ConfigAccess,
    node_did: &str,
    backend_id: &str,
) -> Result<Option<InferenceBackend>> {
    access
        .transact_readonly("test.registry.backend", |txn| {
            Box::pin(async move {
                crate::store::config_client::load_inference_backend_in_txn(
                    txn, node_did, backend_id,
                )
                .await
            })
        })
        .await
}

#[tokio::test]
async fn scoped_backend_replacement_preserves_observations_and_resets_defaults() -> Result<()> {
    let (db, _user, access) =
        crate::test_support::signed_test_node_without_agent("registry-replacement").await;
    let other_db = crate::support::test_db("registry-other-node").await;
    let stranger_db = crate::support::test_db("registry-stranger").await;
    let node = db.node.clone();
    let owner = db.node_identity.did();
    let other_did = other_db.node_identity.did();
    let stranger = stranger_db.node_identity.did();
    crate::test_support::bootstrap_signed_test_node(&access, other_did).await;
    let mut backend = base_backend();
    backend.node_did = owner.to_owned();
    backend.backend_id = "reviewers\"quoted".into();
    backend.max_concurrent = Some(4);
    backend.max_queue_depth = Some(0);
    backend.connect_timeout_secs = Some(3);
    backend.discovery_timeout_secs = Some(9);
    backend.enabled = false;
    backend.tags = vec!["old".into()];
    let doc_id =
        crate::documents::write_inference_backend_document(&access, &backend).await?;
    let catalogs = serde_json::json!([
        {"node_did":null,"observed_at":"2026-01-01T00:00:00Z","models":[{"model_name":"shared","display_name":null,"context_window":null,"max_context_window":null,"max_output_tokens":null,"reasoning_efforts":null}]},
        {"node_did":other_did,"observed_at":"2026-01-02T00:00:00Z","models":[{"model_name":"private","display_name":null,"context_window":null,"max_context_window":null,"max_output_tokens":null,"reasoning_efforts":null}]}
    ]);
    let observation = serde_json::json!({"catalogs":{"entries":catalogs.clone()},"probe_status":"healthy","last_probe":"2026-01-02T00:00:00Z"});
    let row = stored_catalog_row(&access, &backend).await?;
    let observation_doc = row["_docID"]
        .as_str()
        .context("observation document")?
        .to_owned();
    access.transact_write("test.backend.observation", |txn| {
        let doc = observation_doc.clone();
        let observation = observation.clone();
        Box::pin(async move {
            txn.execute_client_signed_update(
                &format!(r#"mutation($input: InferenceBackendObservationMutationInputArg!) {{ update_InferenceBackendObservation(docID: "{}", input: $input) {{ _docID }} }}"#, escape_graphql_string(&doc)),
                &serde_json::json!({"input": observation}),
                gents_protocol::schemas::INFERENCE_BACKEND_OBSERVATION,
            ).await?;
            Ok::<(), anyhow::Error>(())
        })
    }).await?;
    let mut other = base_backend();
    other.node_did = other_did.to_owned();
    other.backend_id = backend.backend_id.clone();
    other.endpoint = "http://other.example/v1".into();
    crate::documents::write_inference_backend_document(&access, &other).await?;
    let mut replacement = base_backend();
    replacement.node_did = owner.to_owned();
    replacement.backend_id = backend.backend_id.clone();
    let replaced_id =
        crate::documents::write_inference_backend_document(&access, &replacement)
            .await?;
    assert_eq!(replaced_id, doc_id);
    let loaded = backend_for_user(&access, owner, &backend.backend_id)
        .await?
        .unwrap();
    assert_eq!(
        serde_json::to_value(&loaded)?,
        serde_json::to_value(&replacement)?
    );
    assert_eq!(loaded.effective_max_concurrent(), 1);
    assert_eq!(loaded.effective_max_queue_depth(), 100);
    let observed = lookup_backend_observation_for_runtime(&node, owner, &backend.backend_id)
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
        observed.catalog_for(Some(other_did))?.unwrap().models[0].model_name,
        "private"
    );
    assert!(observed.catalog_for(Some(stranger))?.is_none());
    let other_loaded = backend_for_user(&access, other_did, &other.backend_id)
        .await?
        .unwrap();
    assert_eq!(other_loaded.endpoint, other.endpoint);
    assert!(backend_for_user(&access, stranger, &backend.backend_id)
        .await?
        .is_none());
    Ok(())
}

fn metadata_input(backend: &InferenceBackend) -> Result<serde_json::Value> {
    let metadata = crate::documents::InferenceBackendMetadata::from_backend(
        backend.clone(),
        crate::documents::BackendAuthSelection::Unauthenticated,
    )?;
    let mut value = serde_json::to_value(metadata)?;
    value["tags"] = serde_json::Value::Null;
    value["creation_nonce"] = uuid::Uuid::new_v4().to_string().into();
    Ok(value)
}

async fn seed_metadata(access: &crate::ConfigAccess, input: serde_json::Value) -> Result<()> {
    let did = input["node_did"]
        .as_str()
        .context("metadata Node")?
        .to_owned();
    access.transact_write("test.registry.seed_metadata", |txn| {
        let input = input.clone();
        let did = did.clone();
        Box::pin(async move {
            let response = txn.execute_client_signed_create(
                "mutation($input: InferenceBackendMetadataMutationInputArg!) { create_InferenceBackendMetadata(input: $input) { _docID } }",
                &serde_json::json!({"input": input}),
                gents_protocol::schemas::INFERENCE_BACKEND_METADATA,
            ).await?;
            let doc = gents_protocol::graphql::extract_mutation_doc_id(&response, "InferenceBackendMetadata")?;
            crate::store::config_client::publish_node_child_in_txn(txn, "InferenceBackendMetadata", &doc, &did).await?;
            Ok::<(), anyhow::Error>(())
        })
    }).await
}

#[tokio::test]
async fn duplicate_backend_owner_keys_fail_without_overwriting_documents() -> Result<()> {
    let (db, _user, access) =
        crate::test_support::signed_test_node_without_agent("registry-duplicate").await;
    let node = db.node.clone();
    let mut backend = base_backend();
    backend.node_did = db.node_identity.did().to_owned();
    crate::documents::write_inference_backend_document(&access, &backend).await?;
    let mut distinct = backend.clone();
    distinct.backend_id = "distinct-control".into();
    seed_metadata(&access, metadata_input(&distinct)?).await?;
    let mut duplicate = backend.clone();
    duplicate.name = "A physically distinct duplicate".into();
    let duplicate_result = seed_metadata(&access, metadata_input(&duplicate)?).await;
    assert!(
        duplicate_result.is_err(),
        "the canonical unique owner/ID index must reject duplicates"
    );
    let loaded = backend_for_user(&access, &backend.node_did, &backend.backend_id)
        .await?
        .expect("original backend remains");
    assert_eq!(loaded.name, backend.name);
    let records = list_enabled_backends_for_runtime(&node, &backend.node_did).await?;
    assert_eq!(records.len(), 2);
    assert_eq!(
        records
            .iter()
            .filter(|row| row.backend_id == backend.backend_id)
            .count(),
        1
    );
    assert!(records
        .iter()
        .any(|row| row.backend_id == distinct.backend_id));
    let response = access
        .execute("{ InferenceBackendObservation { probe_status } }")
        .await?;
    for row in response["data"]["InferenceBackendObservation"]
        .as_array()
        .unwrap()
    {
        assert!(row["probe_status"].is_null());
    }
    Ok(())
}

#[tokio::test]
async fn backend_lists_skip_an_unknown_provider_kind_and_lookups_stay_strict() -> Result<()> {
    let (db, _user, access) =
        crate::test_support::signed_test_node_without_agent("registry-future-kind").await;
    let node = db.node.clone();
    let owner = db.node_identity.did().to_owned();
    let mut backend = base_backend();
    backend.node_did = owner.clone();
    crate::documents::write_inference_backend_document(&access, &backend).await?;
    let mut future = metadata_input(&backend)?;
    future["backend_id"] = "future".into();
    future["provider_kind"] = "FutureProviderKind".into();
    future["endpoint"] = "https://example.invalid/v1".into();
    future["enabled"] = true.into();
    seed_metadata(&access, future).await?;

    let ids = |backends: Vec<InferenceBackend>| {
        backends
            .into_iter()
            .map(|backend| backend.backend_id)
            .collect::<Vec<_>>()
    };
    let response = access
        .execute(&format!(
            "{{ InferenceBackendMetadata {{ _docID {BACKEND_CONFIG_FIELDS} }} }}"
        ))
        .await?;
    let all = decode_backend_records(response.get("data"), true)?
        .into_iter()
        .map(|(_, backend)| backend)
        .collect();
    assert_eq!(ids(all), ["reviewers"]);
    assert_eq!(
        ids(list_enabled_backends_for_runtime(&node, &owner).await?),
        ["reviewers"]
    );
    let error = backend_for_user(&access, &owner, "future")
        .await
        .expect_err("a single lookup stays strict");
    assert!(
        format!("{error:#}").contains("FutureProviderKind"),
        "{error:#}"
    );
    Ok(())
}

async fn record_catalog_for_runtime(
    node: &Arc<EmbeddedNode>,
    backend: &InferenceBackend,
    catalog: BackendModelCatalog,
) -> Result<()> {
    let access =
        crate::inference::auth::credential::runtime_credential_access(node, &backend.node_did)?;
    access
        .transact_write("test.registry.runtime_catalog", |txn| {
            let catalog = catalog.clone();
            Box::pin(async move { record_model_catalog_in_txn(txn, backend, catalog).await })
        })
        .await
}

#[tokio::test]
async fn discovery_rejects_wrong_scope_and_stale_connection_without_losing_catalog() -> Result<()> {
    let (db, _user, access) =
        crate::test_support::signed_test_node_without_agent("registry-catalog").await;
    let node = db.node.clone();
    let mut backend = base_backend();
    backend.node_did = db.node_identity.did().to_owned();
    crate::documents::write_inference_backend_document(&access, &backend).await?;
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
    record_catalog_for_runtime(&node, &backend, catalog.clone()).await?;
    let other_db = crate::support::test_db("registry-other-node").await;
    let mut wrong_scope = catalog.clone();
    wrong_scope.node_did = Some(other_db.node_identity.did().to_owned());
    assert!(record_catalog_for_runtime(&node, &backend, wrong_scope)
        .await
        .is_err());
    let mut older = catalog.clone();
    older.observed_at = "2026-09-09T09:00:00Z".into();
    older.models.clear();
    record_catalog_for_runtime(&node, &backend, older).await?;
    let mut rediscovered = catalog.clone();
    rediscovered.observed_at = "2026-09-09T11:00:00Z".into();
    record_catalog_for_runtime(&node, &backend, rediscovered.clone()).await?;
    assert_eq!(
        lookup_backend_observation_for_runtime(&node, &backend.node_did, &backend.backend_id)
            .await?
            .unwrap()
            .catalogs,
        vec![rediscovered.clone()],
        "the public observation must retain the latest successful discovery"
    );
    let mut changed = backend.clone();
    changed.endpoint = "http://127.0.0.1:9000/v1".into();
    crate::documents::write_inference_backend_document(&access, &changed).await?;
    assert!(record_catalog_for_runtime(&node, &backend, catalog.clone())
        .await
        .is_err());
    assert_eq!(
        lookup_backend_observation_for_runtime(&node, &backend.node_did, &backend.backend_id)
            .await?
            .unwrap()
            .catalogs,
        vec![rediscovered]
    );
    Ok(())
}

#[tokio::test]
async fn catalog_transaction_preserves_explicit_empty_lists_and_rollback() -> Result<()> {
    let (db, _user, access) =
        crate::test_support::signed_test_node_without_agent("registry-catalog").await;
    let node = db.node.clone();
    let mut backend = base_backend();
    backend.node_did = db.node_identity.did().to_owned();
    crate::documents::write_inference_backend_document(&access, &backend).await?;
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
        lookup_backend_observation_for_runtime(&node, &backend.node_did, &backend.backend_id)
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
        lookup_backend_observation_for_runtime(&node, &backend.node_did, &backend.backend_id)
            .await?
            .unwrap()
            .catalogs,
        vec![catalog]
    );
    record_catalog_for_runtime(&node, &backend, empty.clone()).await?;
    assert_eq!(
        lookup_backend_observation_for_runtime(&node, &backend.node_did, &backend.backend_id)
            .await?
            .unwrap()
            .catalogs,
        vec![empty]
    );
    node.shutdown().await.expect("node shutdown");
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
    let (db, _user, access) =
        crate::test_support::signed_test_node_without_agent("registry-operator").await;
    let node = db.node.clone();
    let owner = db.node_identity.did().to_owned();
    let endpoint = spawn_echoing_models_server(SECRET).await;
    let mut claude = base_backend();
    claude.node_did = owner.clone();
    claude.backend_id = "claude".into();
    claude.provider_kind = BackendProviderKind::ClaudeCliSubscription;
    claude.auth = BackendAuth::NodeOAuth { account_ref: None };
    claude.endpoint = endpoint.clone();
    let mut elsewhere = claude.clone();
    elsewhere.backend_id = "claude-elsewhere".into();
    elsewhere.endpoint = "http://127.0.0.1:1/v1".into();
    for backend in [&claude, &elsewhere] {
        crate::documents::write_inference_backend_document(&access, backend).await?;
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

    let observation = lookup_backend_observation_for_runtime(&node, &owner, "claude")
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
        lookup_backend_observation_for_runtime(&node, &owner, "claude-elsewhere")
            .await?
            .unwrap()
            .catalogs
            .is_empty()
    );

    let stored = access
        .execute("{ InferenceBackendObservation { catalogs } }")
        .await?;
    let stored = serde_json::to_string(&stored)?;
    assert!(stored.contains("claude-opus-5-5"));
    assert!(
        !stored.contains(SECRET),
        "credential material persisted: {stored}"
    );
    node.shutdown().await.expect("node shutdown");
    Ok(())
}

async fn stored_catalog_row(
    access: &crate::store::config_client::ConfigAccess,
    backend: &InferenceBackend,
) -> Result<serde_json::Value> {
    let filter = scope_filter(&backend.node_did, &backend.backend_id)?;
    let response = access.execute(&format!(
        "{{ InferenceBackendObservation(filter: {filter}) {{ _docID metadata_doc_id backend_id catalogs catalog_freshness }} }}"
    )).await?;
    Ok(response["data"]["InferenceBackendObservation"][0].clone())
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
    let (db, _user, access) =
        crate::test_support::signed_test_node_without_agent("registry-generated-catalog").await;
    let node = db.node.clone();
    for (index, case) in crate::lean_vocab_test::lean_catalog_refresh_cases()
        .iter()
        .enumerate()
    {
        let mut backend = base_backend();
        backend.node_did = db.node_identity.did().to_owned();
        backend.backend_id = format!("catalog-case-{index}");
        crate::documents::write_inference_backend_document(&access, &backend).await?;
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
            record_catalog_for_runtime(&node, &backend, catalog_action(action, &backend)?).await?;
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
        let projected =
            lookup_backend_observation_for_runtime(&node, &backend.node_did, &backend.backend_id)
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
    node.shutdown().await.expect("node shutdown");
    Ok(())
}

#[tokio::test]
async fn catalog_freshness_survives_reopen_and_adopts_retained_payloads() -> Result<()> {
    let (mut db, user, access) =
        crate::test_support::signed_test_node_without_agent("registry-reopen").await;
    let node = db.node.clone();
    let mut backend = base_backend();
    backend.node_did = db.node_identity.did().to_owned();
    crate::documents::write_inference_backend_document(&access, &backend).await?;
    let case = crate::lean_vocab_test::lean_catalog_refresh_cases()
        .iter()
        .find(|case| case["witness"] == "catalog.delayed-change-after-identical")
        .unwrap();
    let actions = case["actions"].as_array().unwrap();
    let original = catalog_action(&actions[0], &backend)?;
    let row = stored_catalog_row(&access, &backend).await?;
    let observation_doc = row["_docID"]
        .as_str()
        .context("observation document")?
        .to_owned();
    access.transact_write("test.catalog.retained", |txn| {
        let doc = observation_doc.clone();
        let original = original.clone();
        Box::pin(async move {
            txn.execute_client_signed_update(
                &format!(r#"mutation($input: InferenceBackendObservationMutationInputArg!) {{ update_InferenceBackendObservation(docID: "{}", input: $input) {{ _docID }} }}"#, escape_graphql_string(&doc)),
                &serde_json::json!({"input": {"catalogs": {"entries": [original]}}}),
                gents_protocol::schemas::INFERENCE_BACKEND_OBSERVATION,
            ).await?;
            Ok::<(), anyhow::Error>(())
        })
    }).await?;

    let row = stored_catalog_row(&access, &backend).await?;
    assert!(row["catalog_freshness"].is_null());
    assert_eq!(
        decode_backend_observation(row)?.catalogs,
        std::slice::from_ref(&original)
    );
    record_catalog_for_runtime(&node, &backend, original.clone()).await?;
    record_catalog_for_runtime(&node, &backend, catalog_action(&actions[1], &backend)?).await?;
    let stored = stored_catalog_row(&access, &backend).await?;
    assert_eq!(
        serde_json::from_value::<InferenceBackendObservation>(stored.clone())?.catalogs,
        [original]
    );
    drop(access);
    drop(node);
    db.simulate_process_crash().await?;
    let node = db.node.clone();
    record_catalog_for_runtime(&node, &backend, catalog_action(&actions[2], &backend)?).await?;
    let access = db.client_access(&user);
    assert_eq!(
        stored_catalog_row(&access, &backend).await?,
        stored,
        "a delayed result after reopen must not write either payload or freshness"
    );
    let observed =
        lookup_backend_observation_for_runtime(&node, &backend.node_did, &backend.backend_id)
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
    node.shutdown().await.expect("node shutdown");
    Ok(())
}
