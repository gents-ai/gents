use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::document_config::{BackendAuth, InferenceBackend, InferenceBackendObservation};
use serde_json::{json, Value};

use crate::shared::ConfigExportBundle;

pub(super) async fn diagnose_backends(
    access: &ConfigAccess,
    bundle: &ConfigExportBundle,
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
        match result {
            Ok(details) => report
                .as_object_mut()
                .unwrap()
                .extend(details.as_object().unwrap().clone()),
            Err(error) => {
                report["error"] = Value::String(error.to_string());
            }
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
    let query = format!(
        "{{ InferenceBackend(filter: {{agent_did: {{_eq: \"{}\"}}, backend_id: {{_eq: \"{}\"}}}}, limit: 2) {{backend_id catalogs probe_status last_probe}} }}",
        gents::graphql::escape_graphql_string(&backend.agent_did),
        gents::graphql::escape_graphql_string(&backend.backend_id),
    );
    let rows = crate::graphql_rows(access, "InferenceBackend", &query).await?;
    anyhow::ensure!(
        rows.len() == 1,
        "backend observation is missing or ambiguous"
    );
    let observation: InferenceBackendObservation = serde_json::from_value(rows[0].clone())?;
    anyhow::ensure!(
        gents::document_configured_from_fields(
            backend.enabled,
            observation.probe_status.as_deref().unwrap_or("unknown")
        ),
        "backend is disabled or has no healthy probe observation"
    );
    if matches!(backend.auth, BackendAuth::PrincipalOAuth) {
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

const OAUTH_CREDENTIAL_DISCOVERY_NOTE: &str =
    "OAuth credential backend: diagnose uses the runtime probe; use config backend discover-models for explicit discovery";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn oauth_backend_uses_exact_owner_observation_without_discovery() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let backend: InferenceBackend = serde_json::from_value(json!({
            "agent_did": "did:key:owner", "backend_id": "claude-max", "name": "Claude",
            "provider_kind": "ClaudeCliSubscription", "endpoint": "claude-cli://subscription",
            "auth": BackendAuth::PrincipalOAuth,
        }))
        .unwrap();
        for (owner, status) in [
            ("did:key:owner", "healthy"),
            ("did:key:foreign", "unhealthy"),
        ] {
            let mut value = serde_json::to_value(&backend).unwrap();
            value["agent_did"] = json!(owner);
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
        let access = ConfigAccess::Local(node);
        let report = backend_check(&access, &backend, &BTreeSet::from(["claude-sonnet-5"]))
            .await
            .unwrap();
        assert_eq!(report["note"], OAUTH_CREDENTIAL_DISCOVERY_NOTE);
        assert_eq!(report["discovered_models"], json!([]));
        let mut missing = backend.clone();
        missing.agent_did = "did:key:missing".into();
        assert!(backend_check(&access, &missing, &BTreeSet::new())
            .await
            .is_err());
    }
}
