use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Context, Result};
use gents::documents::{BackendAuth, InferenceBackend};
use gents::store::config_client::ConfigAccess;
use serde_json::{json, Value};

use crate::shared::ConfigExportBundle;

pub(super) async fn diagnose_backends(
    access: &ConfigAccess,
    bundle: &ConfigExportBundle,
    accounts: &[gents::inference::auth::credential::AccountSummary],
) -> Vec<Value> {
    let mut models = BTreeMap::<&str, BTreeSet<&str>>::new();
    for profile in &bundle.config.inference_profiles {
        models
            .entry(&profile.backend_id)
            .or_default()
            .insert(&profile.model_name);
    }
    let mut reports = Vec::new();
    for backend in &bundle.config.inference_backends {
        let required = models
            .remove(backend.backend_id.as_str())
            .unwrap_or_default();
        let result = backend_check(access, backend, &required).await;
        let mut report = json!({
            "backend_id": backend.backend_id,
            "provider_kind": backend.provider_kind.as_str(),
            "endpoint": backend.endpoint,
            "enabled": backend.enabled,
            "required_models": required,
            "ok": result.is_ok(),
        });
        if let Some(account) = account_field(backend, accounts) {
            report["account"] = account;
        }
        match result {
            Ok(details) => report
                .as_object_mut()
                .unwrap()
                .extend(details.as_object().unwrap().clone()),
            Err(error) => {
                report["error"] = Value::String(error.to_string());
            }
        }
        let observation =
            crate::shared::load_backend_observation(access, &backend.node_did, &backend.backend_id)
                .await
                .ok();
        let warnings = bundle
            .config
            .inference_profiles
            .iter()
            .filter(|profile| profile.backend_id == backend.backend_id)
            .filter_map(|profile| {
                gents::inference::resolved::unsent_reasoning_effort(
                    backend,
                    profile,
                    observation.as_ref(),
                )
            })
            .collect::<Vec<_>>();
        if !warnings.is_empty() {
            report["warnings"] = json!(warnings);
        }
        reports.push(report);
    }
    for (backend_id, required) in models {
        reports.push(json!({"backend_id": backend_id, "ok": false,
            "required_models": required, "error": "referenced backend is missing"}));
    }
    reports.sort_by(|a, b| a["backend_id"].as_str().cmp(&b["backend_id"].as_str()));
    reports
}

async fn backend_check(
    access: &ConfigAccess,
    backend: &InferenceBackend,
    required: &BTreeSet<&str>,
) -> Result<Value> {
    backend.validate()?;
    let observation =
        crate::shared::load_backend_observation(access, &backend.node_did, &backend.backend_id)
            .await?;
    let oauth = matches!(backend.auth, BackendAuth::NodeOAuth { .. });
    let probe_status = observation.probe_status.as_deref().unwrap_or("unknown");
    let offline_unprobed =
        matches!(access, ConfigAccess::Local(_)) && probe_status == "unknown" && !oauth;
    anyhow::ensure!(
        if offline_unprobed {
            backend.enabled
        } else {
            gents::document_configured_from_fields(backend.enabled, probe_status)
        },
        "backend is disabled or has no healthy probe observation"
    );
    if oauth {
        return Ok(json!({"probe_status": observation.probe_status,
            "note": OAUTH_CREDENTIAL_DISCOVERY_NOTE, "discovered_models": []}));
    }
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(
            backend.connect_timeout_secs.unwrap_or(10) as u64,
        ))
        .timeout(Duration::from_secs(
            backend.discovery_timeout_secs.unwrap_or(10) as u64,
        ))
        .build()
        .context("building backend discovery client")?;
    let key = backend.auth.resolve_api_key()?;
    let discovered = gents::discover_backend_models(
        &client,
        backend.provider_kind,
        &backend.endpoint,
        key.as_deref(),
        None,
    )
    .await
    .context("backend model discovery failed")?;
    let names = discovered
        .iter()
        .map(|model| model.model_name.as_str())
        .collect::<BTreeSet<_>>();
    let missing = required.difference(&names).copied().collect::<Vec<_>>();
    anyhow::ensure!(
        missing.is_empty(),
        "backend is missing required models: {}",
        missing.join(", ")
    );
    Ok(json!({"probe_status": observation.probe_status, "discovered_models": names, "error": null}))
}

fn account_field(
    backend: &InferenceBackend,
    accounts: &[gents::inference::auth::credential::AccountSummary],
) -> Option<Value> {
    gents::inference::auth::credential::backend_account(backend, accounts)
        .map(|account| account.map_or(Value::Null, |account| json!(account.label)))
}

const OAUTH_CREDENTIAL_DISCOVERY_NOTE: &str =
    "OAuth credential backend: diagnose uses the runtime probe; use config backend discover-models for explicit discovery";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn enabled_backend_is_diagnosed_by_discovery_without_runtime_probe() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let app = axum::Router::new().route(
            "/v1/models",
            axum::routing::get({
                let calls = calls.clone();
                move || {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({"data":[{"id":"local-model"}]}))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let backend: InferenceBackend = serde_json::from_value(json!({
            "node_did":"did:key:owner", "backend_id":"offline-backend", "name":"Local",
            "provider_kind":"OpenAiCompatible", "endpoint":format!("http://{address}/v1"),
            "auth":{"kind":"unauthenticated"}, "enabled":true,
        }))
        .unwrap();
        let mut stored = serde_json::to_value(&backend).unwrap();
        stored["probe_status"] = json!("unknown");
        let input = gents_protocol::graphql::graphql_input_literal(&stored).unwrap();
        let access = ConfigAccess::local(node.clone());
        access
            .write(
                "test.diagnose.backend",
                &format!("mutation {{ create_InferenceBackend(input: {input}) {{_docID}} }}"),
            )
            .await
            .unwrap();
        let report = backend_check(&access, &backend, &BTreeSet::from(["local-model"]))
            .await
            .unwrap();
        assert_eq!(report["probe_status"], "unknown");
        assert_eq!(report["discovered_models"], json!(["local-model"]));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let observed = crate::shared::load_backend_observation(
            &access,
            &backend.node_did,
            &backend.backend_id,
        )
        .await
        .unwrap();
        assert_eq!(observed.probe_status.as_deref(), Some("unknown"));
        let mut disabled = backend.clone();
        disabled.enabled = false;
        assert!(backend_check(&access, &disabled, &BTreeSet::new())
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
        let _ = server.await;
        assert!(
            backend_check(&access, &backend, &BTreeSet::from(["local-model"]))
                .await
                .is_err()
        );
        access.write("test.diagnose.unhealthy", r#"mutation { update_InferenceBackend(filter: {backend_id: {_eq: "offline-backend"}}, input: {probe_status: "unhealthy"}) {_docID} }"#).await.unwrap();
        let error = backend_check(&access, &backend, &BTreeSet::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no healthy probe observation"));
        node.shutdown().await;
    }

    #[tokio::test]
    async fn oauth_backend_uses_exact_owner_observation_without_discovery() {
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let backend: InferenceBackend = serde_json::from_value(json!({
            "node_did": "did:key:owner", "backend_id": "claude-max", "name": "Claude",
            "provider_kind": "ClaudeCliSubscription", "endpoint": "claude-cli://subscription",
            "auth": BackendAuth::NodeOAuth { account_ref: None },
        }))
        .unwrap();
        for (owner, status) in [
            ("did:key:owner", "healthy"),
            ("did:key:foreign", "unhealthy"),
        ] {
            let mut value = serde_json::to_value(&backend).unwrap();
            value["node_did"] = json!(owner);
            value["probe_status"] = json!(status);
            value["enabled"] = json!(true);
            let input = gents_protocol::graphql::graphql_input_literal(&value).unwrap();
            let result = node
                .execute(&format!(
                    "mutation {{ create_InferenceBackend(input: {input}) {{_docID}} }}"
                ))
                .await;
            assert!(!result.has_errors(), "{:?}", result.errors);
        }
        let access = ConfigAccess::local(node);
        let report = backend_check(&access, &backend, &BTreeSet::from(["claude-sonnet-5"]))
            .await
            .unwrap();
        assert_eq!(report["note"], OAUTH_CREDENTIAL_DISCOVERY_NOTE);
        assert_eq!(report["discovered_models"], json!([]));
        let mut missing = backend.clone();
        missing.node_did = "did:key:missing".into();
        assert!(backend_check(&access, &missing, &BTreeSet::new())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn diagnose_reports_an_xai_effort_that_will_not_be_sent() {
        let owner = "did:key:owner";
        let access = crate::shared::test_support::seed_unsent_xai_effort(owner).await;
        let mut backend = crate::shared::test_support::xai_effort_backend(owner);
        let object = backend.as_object_mut().unwrap();
        object.remove("catalogs");
        object.remove("probe_status");
        let bundle: ConfigExportBundle = serde_json::from_value(json!({
            "format": "test", "node_did": owner, "exported_at": "2026-01-01T00:00:00Z",
            "access_mode": "local", "node": {"node_did": owner},
            "inference_backends": [backend],
            "inference_profiles": [crate::shared::test_support::xai_effort_profile(owner)],
        }))
        .unwrap();
        let reports = diagnose_backends(&access, &bundle, &[]).await;
        let warnings = reports[0]["warnings"].as_array().expect("warnings");
        assert_eq!(warnings.len(), 1, "{reports:#?}");
        assert!(
            warnings[0].as_str().unwrap().contains("profile grok"),
            "{warnings:?}"
        );
    }
}
