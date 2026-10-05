use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::document_config::{BackendAuth, InferenceBackend};
use serde_json::{json, Value};

use crate::shared::ConfigExportBundle;

pub(super) async fn diagnose_backends(
    access: &ConfigAccess,
    bundle: &ConfigExportBundle,
    accounts: &[gents::oauth_credential::AccountSummary],
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
        let observation = crate::shared::load_backend_observation(
            access,
            &backend.agent_did,
            &backend.backend_id,
        )
        .await
        .ok();
        let warnings = bundle
            .config
            .inference_profiles
            .iter()
            .filter(|profile| profile.backend_id == backend.backend_id)
            .filter_map(|profile| {
                gents::config::unsent_reasoning_effort(backend, profile, observation.as_ref())
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
        crate::shared::load_backend_observation(access, &backend.agent_did, &backend.backend_id)
            .await?;
    anyhow::ensure!(
        gents::document_configured_from_fields(
            backend.enabled,
            observation.probe_status.as_deref().unwrap_or("unknown")
        ),
        "backend is disabled or has no healthy probe observation"
    );
    if matches!(backend.auth, BackendAuth::PrincipalOAuth { .. }) {
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

/// The `account` a backend report names: the label of the account a principal
/// OAuth backend runs on, `null` when that account is not on this node; absent
/// for a backend that uses no account.
fn account_field(
    backend: &InferenceBackend,
    accounts: &[gents::oauth_credential::AccountSummary],
) -> Option<Value> {
    gents::oauth_credential::backend_account(backend, accounts)
        .map(|account| account.map_or(Value::Null, |account| json!(account.label)))
}

const OAUTH_CREDENTIAL_DISCOVERY_NOTE: &str =
    "OAuth credential backend: diagnose uses the runtime probe; use config backend discover-models for explicit discovery";

#[cfg(test)]
mod tests {
    use super::*;
    use gents::oauth_credential::AccountSummary;
    use std::sync::Arc;

    fn claude_backend(account_ref: Option<&str>) -> InferenceBackend {
        serde_json::from_value(json!({
            "agent_did": "did:key:owner", "backend_id": "claude", "name": "Claude",
            "provider_kind": "ClaudeCliSubscription", "endpoint": "claude-cli://subscription",
            "auth": BackendAuth::PrincipalOAuth { account_ref: account_ref.map(str::to_owned) },
        }))
        .unwrap()
    }

    fn claude_account(account_ref: Option<&str>, label: &str) -> AccountSummary {
        AccountSummary {
            credential_id: format!(
                "claude-subscription:did:key:owner{}",
                account_ref.unwrap_or("")
            ),
            provider: "claude-subscription".into(),
            account_ref: account_ref.map(str::to_owned),
            label: label.into(),
            identity: Some("identity-private".into()),
            plan: None,
            enabled: true,
            default: false,
            access_token_expires_at: chrono::Utc::now(),
            connected_at: None,
        }
    }

    #[test]
    fn oauth_backend_rows_name_their_account() {
        let accounts = [
            claude_account(None, "Personal"),
            claude_account(Some("acct-2"), "Work"),
        ];
        assert_eq!(
            account_field(&claude_backend(Some("acct-2")), &accounts),
            Some(json!("Work"))
        );
        assert_eq!(
            account_field(&claude_backend(None), &accounts),
            Some(json!("Personal"))
        );
        let mut keyed: InferenceBackend = claude_backend(None);
        keyed.provider_kind = gents::BackendProviderKind::OpenAiCompatible;
        keyed.auth = BackendAuth::Unauthenticated;
        assert_eq!(account_field(&keyed, &accounts), None);
    }

    #[test]
    fn an_oauth_backend_without_a_local_account_names_none() {
        let accounts = [claude_account(Some("acct-2"), "Work")];
        assert_eq!(
            account_field(&claude_backend(Some("acct-other")), &accounts),
            Some(Value::Null)
        );
        assert_eq!(
            account_field(&claude_backend(None), &accounts),
            Some(Value::Null)
        );
    }

    #[tokio::test]
    async fn oauth_backend_uses_exact_owner_observation_without_discovery() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let backend: InferenceBackend = serde_json::from_value(json!({
            "agent_did": "did:key:owner", "backend_id": "claude-max", "name": "Claude",
            "provider_kind": "ClaudeCliSubscription", "endpoint": "claude-cli://subscription",
            "auth": BackendAuth::PrincipalOAuth { account_ref: None },
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

    #[tokio::test]
    async fn diagnose_reports_an_xai_effort_that_will_not_be_sent() {
        let owner = "did:key:owner";
        let access = crate::shared::test_support::seed_unsent_xai_effort(owner).await;
        let mut backend = crate::shared::test_support::xai_effort_backend(owner);
        let object = backend.as_object_mut().unwrap();
        object.remove("catalogs");
        object.remove("probe_status");
        let bundle: ConfigExportBundle = serde_json::from_value(json!({
            "format": "test", "agent_did": owner, "exported_at": "2026-01-01T00:00:00Z",
            "access_mode": "local", "agent_principal": {"agent_did": owner},
            "inference_backends": [backend],
            "inference_profiles": [crate::shared::test_support::xai_effort_profile(owner)],
        }))
        .unwrap();
        let reports = diagnose_backends(&access, &bundle).await;
        let warnings = reports[0]["warnings"].as_array().expect("warnings");
        assert_eq!(warnings.len(), 1, "{reports:#?}");
        assert!(
            warnings[0].as_str().unwrap().contains("profile grok"),
            "{warnings:?}"
        );
    }
}
