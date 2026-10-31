use crate::cli::*;
use crate::shared::ResolvedBackendConfig;
use crate::{resolve_node_did, BackendResolutionMode};
use anyhow::{Context, Result};
use gents::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, set_backend_rotation,
    BackendRotation, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::document_config::{BackendAuth, InferenceBackend};
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
        .transact_write("cli.backend.set", |txn| {
            let backend = &backend;
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan).await?;
                Ok::<_, anyhow::Error>(
                    read_desired_state_record_in_txn(
                        txn,
                        Collection::InferenceBackend,
                        &backend.node_did,
                        &backend.backend_id,
                    )
                    .await?
                    .context("replaced backend missing")?
                    .0,
                )
            })
        })
        .await?;
    crate::print_json(
        &json!({"doc_id":doc_id,"node_did":backend.node_did,"backend_id":backend.backend_id,"provider_kind":backend.provider_kind.as_str(),"endpoint":backend.endpoint}),
    )
}

fn pool_rotation(args: &BackendPoolArgs) -> BackendRotation {
    if args.single {
        return BackendRotation::Single;
    }
    BackendRotation::Pool {
        allow_overage: match (args.allow_overage, args.no_overage) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        },
    }
}

async fn pool_output(
    access: &ConfigAccess,
    node_did: &str,
    backend_id: &str,
    rotation: BackendRotation,
) -> Result<serde_json::Value> {
    let outcome = set_backend_rotation(access, node_did, backend_id, rotation).await?;
    Ok(json!({
        "backend_id": outcome.backend.backend_id,
        "auth": outcome.backend.auth,
        "accounts": outcome.members,
    }))
}

pub(super) async fn backend_pool(args: BackendPoolArgs) -> Result<()> {
    let (access, node_did) =
        super::profile::target(args.home.as_deref(), args.graphql.as_deref()).await?;
    crate::print_json(
        &pool_output(&access, &node_did, &args.backend_id, pool_rotation(&args)).await?,
    )
}

pub(super) async fn backend_discover_models(args: BackendDiscoverModelsArgs) -> Result<()> {
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
    let (oauth_credential, oauth_node_did) = if target.provider_kind.is_node_scoped_oauth() {
        let account_ref = stored
            .as_ref()
            .and_then(|backend| backend.auth.oauth_account_ref());
        let (credential, owner) =
            load_oauth_credential_for_discovery(&args, target.provider_kind, account_ref).await?;
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
                oauth_node_did.as_deref().unwrap_or(""),
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
                oauth_node_did.as_deref().unwrap_or(""),
                gents::xai_grok_oauth::XAI_OAUTH_PROVIDER,
                &gents::oauth_credential::OAuthAuthProblem::Expired,
            );
            anyhow::bail!("{error:#}\n{guidance}");
        }
        Err(error)
            if target.provider_kind == BackendProviderKind::XaiGrokOAuth
                && discovery_error_is_client_version_gate(&error) =>
        {
            anyhow::bail!(
                "{error:#}\n{}",
                gents::xai_grok_oauth::grok_client_version_gate_guidance()
            );
        }
        Err(error)
            if target.provider_kind == BackendProviderKind::ClaudeCliSubscription
                && discovery_error_is_auth(&error) =>
        {
            let guidance = gents::claude_oauth::classify_claude_auth_error(
                oauth_node_did.as_deref().unwrap_or(""),
                gents::claude_oauth::CLAUDE_OAUTH_PROVIDER,
                &gents::oauth_credential::OAuthAuthProblem::Expired,
            );
            anyhow::bail!("{error:#}\n{guidance}");
        }
        Err(error) => return Err(error),
    };

    let catalog_written = if let Some(backend) = stored.as_ref() {
        let (access, _) =
            crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
        gents::record_discovered_catalog_on(&access, backend, discovered_models.clone()).await?;
        true
    } else {
        false
    };
    crate::print_json(&json!({
        "backend_id":args.backend_id,"backend_preset":args.backend_preset.map(BackendPresetArg::as_str),
        "provider_kind":target.provider_kind.as_str(),"endpoint":target.endpoint,
        "api_key":target.api_key.as_ref().map(|_|"<redacted>"),"api_key_env_var":target.api_key_env_var,
        "discovered_models":discovered_models,"catalog_written":catalog_written,
    }))
}

async fn load_oauth_credential_for_discovery(
    args: &BackendDiscoverModelsArgs,
    provider_kind: BackendProviderKind,
    account_ref: Option<&str>,
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
    let node_did = resolve_node_did(args.home.as_deref(), args.node_did.as_deref())?;
    let (access, _) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let credential = gents::oauth_credential::resolve_oauth_credential(
        &access,
        &node_did,
        provider,
        gents::oauth_credential::AccountPick::Reference(account_ref),
    )
    .await?;
    Ok((credential, node_did))
}

fn discovery_error_is_auth(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<gents::backend_provider::ModelDiscoveryHttpError>()
            .is_some_and(|http| http.is_auth())
    })
}

fn discovery_error_is_client_version_gate(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<gents::backend_provider::ModelDiscoveryHttpError>()
            .is_some_and(|http| http.is_client_version_gate())
    })
}

fn stored_backend_target(backend: &InferenceBackend) -> Result<ResolvedBackendConfig> {
    backend.validate()?;
    Ok(ResolvedBackendConfig {
        provider_kind: backend.provider_kind,
        openai_wire_api: backend.openai_wire_api,
        endpoint: backend.endpoint.clone(),
        api_key: match &backend.auth {
            auth if auth.uses_node_oauth() => None,
            auth => auth.probe_key()?,
        },
        api_key_env_var: match &backend.auth {
            BackendAuth::Environment { variable } => Some(variable.clone()),
            _ => None,
        },
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
        let owner = resolve_node_did(args.home.as_deref(), args.node_did.as_deref())?;
        let (access, _) =
            crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
        let backend = access
            .transact_write("cli.backend.discovery_target", |txn| {
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
                    Ok::<_, anyhow::Error>(serde_json::from_value::<InferenceBackend>(value)?)
                })
            })
            .await?;
        let target = stored_backend_target(&backend)?;
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
    use gents::oauth_credential::{set_account_enabled, store_sign_in};

    const DID: &str = "did:key:z6MkTestBackendPool";
    const CLAUDE: &str = "claude-subscription";

    fn claude_sign_in(who: &str) -> gents::oauth_credential::OAuthCredential {
        gents::claude_oauth::credential_from_login_tokens(
            DID,
            CLAUDE,
            &gents::claude_oauth::ClaudeLoginTokens {
                access_token: "access-SECRET".into(),
                refresh_token: format!("refresh-SECRET-{who}"),
                expires_in: Some(3600),
                scope: None,
                account_id: Some(format!("IDENTITY-{who}")),
                organization_uuid: Some("org-1".into()),
                account_uuid: Some(format!("account-{who}")),
            },
            chrono::Utc::now(),
        )
    }

    async fn seeded() -> ConfigAccess {
        let node = std::sync::Arc::new(
            crate::shared::test_support::node_builder()
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::local(node);
        for (id, kind, endpoint, auth) in [
            (
                "claude",
                "ClaudeCliSubscription",
                "claude-cli://subscription",
                json!({"kind": "node_oauth"}),
            ),
            (
                "codex",
                "ChatGptCodex",
                "https://chatgpt.com/backend-api/codex",
                json!({"kind": "node_oauth"}),
            ),
            (
                "openai",
                "OpenAiCompatible",
                "http://127.0.0.1:1/v1",
                json!({"kind": "api_key", "key": "key-SECRET"}),
            ),
        ] {
            let backend = serde_json::from_value(json!({
                "node_did": DID, "backend_id": id, "name": id,
                "provider_kind": kind, "endpoint": endpoint, "auth": auth,
            }))
            .unwrap();
            gents::config_client::write_inference_backend_document(&access, &backend)
                .await
                .unwrap();
        }
        access
    }

    async fn stored_auth(access: &ConfigAccess, backend_id: &str) -> BackendAuth {
        access
            .transact_readonly("test.backend.pool.read", |txn| {
                Box::pin(async move {
                    gents::config_client::load_inference_backend_in_txn(txn, DID, backend_id)
                        .await?
                        .context("backend missing")
                })
            })
            .await
            .unwrap()
            .auth
    }

    fn pool(allow_overage: Option<bool>) -> BackendRotation {
        BackendRotation::Pool { allow_overage }
    }

    #[tokio::test]
    async fn a_single_account_backend_becomes_a_pool_with_overage_off() {
        let access = seeded().await;
        let output = pool_output(&access, DID, "claude", pool(None))
            .await
            .unwrap();
        assert_eq!(
            output["auth"],
            json!({"kind": "node_oauth_pool", "allow_overage": false})
        );
        assert_eq!(
            stored_auth(&access, "claude").await,
            BackendAuth::NodeOAuthPool {
                allow_overage: false
            }
        );
    }

    #[tokio::test]
    async fn overage_is_changed_by_a_flag_and_kept_without_one() {
        let access = seeded().await;
        pool_output(&access, DID, "claude", pool(Some(true)))
            .await
            .unwrap();
        let kept = pool_output(&access, DID, "claude", pool(None))
            .await
            .unwrap();
        assert_eq!(kept["auth"]["allow_overage"], json!(true));
        let off = pool_output(&access, DID, "claude", pool(Some(false)))
            .await
            .unwrap();
        assert_eq!(off["auth"]["allow_overage"], json!(false));
    }

    #[tokio::test]
    async fn single_keeps_the_account_a_one_account_backend_already_uses() {
        let access = seeded().await;
        let pinned = serde_json::from_value(json!({
            "node_did": DID, "backend_id": "claude", "name": "claude",
            "provider_kind": "ClaudeCliSubscription", "endpoint": "claude-cli://subscription",
            "auth": {"kind": "node_oauth", "account_ref": "pinned"},
        }))
        .unwrap();
        gents::config_client::write_inference_backend_document(&access, &pinned)
            .await
            .unwrap();
        pool_output(&access, DID, "claude", BackendRotation::Single)
            .await
            .unwrap();
        assert_eq!(
            stored_auth(&access, "claude").await,
            BackendAuth::NodeOAuth {
                account_ref: Some("pinned".into())
            }
        );
    }

    #[tokio::test]
    async fn single_returns_to_the_default_account_and_lists_none() {
        let access = seeded().await;
        store_sign_in(&access, claude_sign_in("a"), None)
            .await
            .unwrap();
        pool_output(&access, DID, "claude", pool(Some(true)))
            .await
            .unwrap();
        let output = pool_output(&access, DID, "claude", BackendRotation::Single)
            .await
            .unwrap();
        assert_eq!(output["auth"], json!({"kind": "node_oauth"}));
        assert_eq!(output["accounts"], json!([]));
        assert_eq!(
            stored_auth(&access, "claude").await,
            BackendAuth::NodeOAuth { account_ref: None }
        );
    }

    #[tokio::test]
    async fn a_non_claude_backend_is_refused_and_left_unchanged() {
        let access = seeded().await;
        let error = pool_output(&access, DID, "openai", pool(None))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("subscription"), "{error}");
        assert!(matches!(
            stored_auth(&access, "openai").await,
            BackendAuth::ApiKey { .. }
        ));
        let codex = pool_output(&access, DID, "codex", pool(None))
            .await
            .unwrap_err()
            .to_string();
        assert!(codex.contains("rotate Claude"), "{codex}");
        assert_eq!(
            stored_auth(&access, "codex").await,
            BackendAuth::NodeOAuth { account_ref: None }
        );
        let missing = pool_output(&access, DID, "nope", pool(None))
            .await
            .unwrap_err()
            .to_string();
        assert!(missing.contains("does not exist"), "{missing}");
    }

    #[tokio::test]
    async fn members_are_the_enabled_accounts_in_rotation_order() {
        let access = seeded().await;
        let a = store_sign_in(&access, claude_sign_in("a"), Some("label-a"))
            .await
            .unwrap();
        let b = store_sign_in(&access, claude_sign_in("b"), Some("label-b"))
            .await
            .unwrap();
        let c = store_sign_in(&access, claude_sign_in("c"), Some("label-c"))
            .await
            .unwrap();
        set_account_enabled(&access, DID, &b.credential.credential_id, false)
            .await
            .unwrap();
        let output = pool_output(&access, DID, "claude", pool(None))
            .await
            .unwrap();
        assert_eq!(
            output["accounts"],
            json!([
                {"credential_id": a.credential.credential_id, "label": "label-a"},
                {"credential_id": c.credential.credential_id, "label": "label-c"},
            ])
        );
        assert!(!output.to_string().contains("SECRET"));
    }

    #[test]
    fn the_flags_choose_the_rotation() {
        let args = |allow_overage, no_overage, single| BackendPoolArgs {
            backend_id: "claude".into(),
            allow_overage,
            no_overage,
            single,
            home: None,
            graphql: None,
        };
        assert_eq!(pool_rotation(&args(false, false, false)), pool(None));
        assert_eq!(pool_rotation(&args(true, false, false)), pool(Some(true)));
        assert_eq!(pool_rotation(&args(false, true, false)), pool(Some(false)));
        assert_eq!(
            pool_rotation(&args(false, false, true)),
            BackendRotation::Single
        );
    }
}
