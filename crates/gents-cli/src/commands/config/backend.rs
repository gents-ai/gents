use crate::cli::*;
use crate::shared::ResolvedBackendConfig;
use crate::{resolve_agent_did, BackendResolutionMode};
use anyhow::{Context, Result};
use gents::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use gents::document_config::{BackendAuth, BackendModelCatalog, InferenceBackend};
use gents::{discover_backend_models, BackendProviderKind, Collection};
use serde_json::json;

fn backend_plan(contents: &[u8]) -> Result<(InferenceBackend, DesiredStateApplyPlan)> {
    let backend: InferenceBackend =
        serde_json::from_slice(contents).context("decoding canonical InferenceBackend document")?;
    backend.validate()?;
    let value = serde_json::to_value(&backend)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::InferenceBackend,
        add: value.clone(),
        update: value,
    }])?;
    Ok((backend, plan))
}

pub(super) async fn backend_set(args: BackendSetArgs) -> Result<()> {
    let (backend, plan) = backend_plan(
        &std::fs::read(&args.file).with_context(|| format!("reading {}", args.file.display()))?,
    )?;
    let (access, _) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let doc_id = access
        .transact("cli.backend.set", |txn| {
            let backend = &backend;
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan).await?;
                Ok(read_desired_state_record_in_txn(
                    txn,
                    Collection::InferenceBackend,
                    &backend.agent_did,
                    &backend.backend_id,
                )
                .await?
                .context("replaced backend missing")?
                .0)
            })
        })
        .await?;
    crate::print_json(
        &json!({"doc_id":doc_id,"agent_did":backend.agent_did,"backend_id":backend.backend_id,"provider_kind":backend.provider_kind.as_str(),"endpoint":backend.endpoint}),
    )
}

pub(super) async fn backend_discover_models(args: BackendDiscoverModelsArgs) -> Result<()> {
    anyhow::ensure!(
        !args.write || args.backend_id.is_some(),
        "--write requires --backend-id"
    );
    let (stored, target) = resolve_backend_discovery_target(&args).await?;
    let timeout = std::time::Duration::from_secs(
        stored
            .as_ref()
            .and_then(|backend| backend.discovery_timeout_secs)
            .unwrap_or(10) as u64,
    );
    let connect = std::time::Duration::from_secs(
        stored
            .as_ref()
            .and_then(|backend| backend.connect_timeout_secs)
            .unwrap_or(10) as u64,
    )
    .min(timeout);
    let client = reqwest::Client::builder()
        .connect_timeout(connect)
        .timeout(timeout)
        .build()
        .context("building backend discovery client")?;
    let (oauth_credential, oauth_agent_did) = if target.provider_kind.is_agent_scoped_oauth() {
        let (credential, owner) =
            load_oauth_credential_for_discovery(&args, target.provider_kind).await?;
        (credential, Some(owner))
    } else {
        (None, None)
    };
    let discovered_models = match discover_backend_models(
        &client,
        target.provider_kind,
        &target.endpoint,
        target.api_key.as_deref(),
        oauth_credential.as_ref(),
    )
    .await
    {
        Ok(models) => models,
        Err(error)
            if target.provider_kind == BackendProviderKind::ChatGptCodex
                && discovery_error_is_auth(&error) =>
        {
            let guidance = gents::oauth_credential::classify_chatgpt_auth_error(
                oauth_agent_did.as_deref().unwrap_or(""),
                gents::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
                &gents::oauth_credential::OAuthAuthProblem::Expired,
            );
            anyhow::bail!("{error:#}\n{guidance}");
        }
        Err(error)
            if target.provider_kind == BackendProviderKind::XaiGrokOAuth
                && discovery_error_is_auth(&error) =>
        {
            let guidance = gents::xai_grok_oauth::classify_xai_auth_error(
                oauth_agent_did.as_deref().unwrap_or(""),
                gents::xai_grok_oauth::XAI_OAUTH_PROVIDER,
                &gents::oauth_credential::OAuthAuthProblem::Expired,
            );
            anyhow::bail!("{error:#}\n{guidance}");
        }
        Err(error)
            if target.provider_kind == BackendProviderKind::ClaudeCliSubscription
                && discovery_error_is_auth(&error) =>
        {
            let guidance = gents::claude_oauth::classify_claude_auth_error(
                oauth_agent_did.as_deref().unwrap_or(""),
                gents::claude_oauth::CLAUDE_OAUTH_PROVIDER,
                &gents::oauth_credential::OAuthAuthProblem::Expired,
            );
            anyhow::bail!("{error:#}\n{guidance}");
        }
        Err(error) => return Err(error),
    };

    let models_written = if args.write {
        let backend = stored
            .as_ref()
            .context("catalog publication needs a stored backend")?;
        let catalog = BackendModelCatalog {
            agent_did: matches!(backend.auth, BackendAuth::PrincipalOAuth)
                .then(|| backend.agent_did.clone()),
            observed_at: chrono::Utc::now().to_rfc3339(),
            models: discovered_models.clone(),
        };
        let (access, _) =
            crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
        access
            .transact("cli.backend.discovery", |txn| {
                let catalog = catalog.clone();
                Box::pin(
                    async move { gents::record_model_catalog_in_txn(txn, backend, catalog).await },
                )
            })
            .await?;
        discovered_models.len()
    } else {
        0
    };
    crate::print_json(&json!({
        "backend_id":args.backend_id,"backend_preset":args.backend_preset.map(BackendPresetArg::as_str),
        "provider_kind":target.provider_kind.as_str(),"endpoint":target.endpoint,
        "api_key":target.api_key.as_ref().map(|_|"<redacted>"),"api_key_env_var":target.api_key_env_var,
        "discovered_models":discovered_models,"models_written":models_written,"catalog_written":args.write,
    }))
}

async fn load_oauth_credential_for_discovery(
    args: &BackendDiscoverModelsArgs,
    provider_kind: BackendProviderKind,
) -> Result<(Option<gents::oauth_credential::OAuthCredential>, String)> {
    let (provider, _login) = match provider_kind {
        BackendProviderKind::ChatGptCodex => (
            gents::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
            "gents codex-login",
        ),
        BackendProviderKind::XaiGrokOAuth => (
            gents::xai_grok_oauth::XAI_OAUTH_PROVIDER,
            "gents grok-login",
        ),
        BackendProviderKind::ClaudeCliSubscription => (
            gents::claude_oauth::CLAUDE_OAUTH_PROVIDER,
            "gents claude-login",
        ),
        _ => anyhow::bail!("load_oauth_credential_for_discovery called for non-OAuth provider"),
    };
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let (access, _) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let credential =
        crate::commands::codex_auth_probe::load_oauth_credential(&access, &agent_did, provider)
            .await?;
    Ok((credential, agent_did))
}

/// Rewrites `models[]` on the stored backend document and nothing else.
async fn write_discovered_models(
    graphql: &str,
    backend_id: &str,
    models: &[String],
) -> Result<usize> {
    let models_field = string_list_field("models", models)
        .ok_or_else(|| anyhow::anyhow!("backend models field could not be rendered"))?;
    let mutation = format!(
        r#"mutation {{
            update_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{}" }} }},
                input: {{ {} }}
            ) {{ _docID }}
        }}"#,
        escape_graphql_string(backend_id),
        models_field,
    );
    let response = ConfigAccess::Graphql(graphql.to_string())
        .write("config.inference_backend.models.update", &mutation)
        .await?;
    extract_mutation_doc_id(&response, "InferenceBackend")
        .with_context(|| format!("updating models on backend {backend_id}"))?;
    Ok(models.len())
}

/// Whether a model-discovery error is an authentication failure (HTTP 401/403), so ChatGptCodex
/// discovery can append re-login guidance the bare error omits. Inspects the typed status carried
/// by [`ModelDiscoveryHttpError`] rather than scraping the rendered message (which also contains the
/// endpoint URL and response body, and could otherwise match "401"/"403" spuriously).
fn discovery_error_is_auth(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<gents::backend_provider::ModelDiscoveryHttpError>()
            .is_some_and(|http| http.is_auth())
    })
}

async fn resolve_backend_discovery_target(
    args: &BackendDiscoverModelsArgs,
) -> Result<(Option<InferenceBackend>, ResolvedBackendConfig)> {
    if let Some(id) = args.backend_id.as_deref() {
        anyhow::ensure!(
            args.backend_preset.is_none()
                && args.provider_kind.is_none()
                && args.endpoint.is_none()
                && args.api_key.is_none()
                && args.api_key_env_var.is_none(),
            "--backend-id uses stored configuration; explicit provider, endpoint and auth flags cannot override it"
        );
        let owner = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
        let (access, _) =
            crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
        let backend = access
            .transact("cli.backend.discovery_target", |txn| {
                let owner = &owner;
                Box::pin(async move {
                    let (_, value) = read_desired_state_record_in_txn(
                        txn,
                        Collection::InferenceBackend,
                        owner,
                        id,
                    )
                    .await?
                    .context("backend absent in selected owner")?;
                    Ok(serde_json::from_value::<InferenceBackend>(value)?)
                })
            })
            .await?;
        backend.validate()?;
        let target = ResolvedBackendConfig {
            provider_kind: backend.provider_kind,
            openai_wire_api: backend.openai_wire_api,
            endpoint: backend.endpoint.clone(),
            api_key: backend.auth.resolve_api_key()?,
            api_key_env_var: match &backend.auth {
                BackendAuth::Environment { variable } => Some(variable.clone()),
                _ => None,
            },
        };
        return Ok((Some(backend), target));
    }
    let mut target = crate::resolve_helpers::resolve_backend_config_with_preset(
        args.backend_preset,
        args.endpoint.as_deref(),
        args.provider_kind.as_deref(),
        None,
        args.api_key.as_deref(),
        args.api_key_env_var.as_deref(),
        BackendResolutionMode::ConfigWrite,
    )?;
    if target.api_key.is_none() {
        if let Some(variable) = &target.api_key_env_var {
            target.api_key = BackendAuth::Environment {
                variable: variable.clone(),
            }
            .resolve_api_key()?;
        }
    }
    Ok((None, target))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_oauth_backend_retains_defaults_without_fake_catalog() {
        let (backend,plan)=backend_plan(br#"{"agent_did":"owner","backend_id":"claude","name":"Claude","provider_kind":"ClaudeCliSubscription","endpoint":"https://api.anthropic.com","auth":{"kind":"principal_o_auth"}}"#).unwrap();
        assert_eq!(backend.auth, BackendAuth::PrincipalOAuth);
        assert_eq!(backend.max_concurrent, None);
        assert!(plan.documents()[0].add.get("catalogs").is_none());
        assert!(plan.documents()[0].add.get("models").is_none());
    }
    #[test]
    fn retired_auth_and_observation_fields_are_not_config() {
        for field in ["api_key", "models", "probe_status"] {
            let mut value = json!({"agent_did":"owner","backend_id":"local","name":"Local","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}});
            value[field] = json!("invalid");
            assert!(backend_plan(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }
}
