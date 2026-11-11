use super::inference_failure::InferenceFailure;
use crate::cli::*;
use crate::shared::ResolvedBackendConfig;
use crate::BackendResolutionMode;
use anyhow::{Context, Result};
use gents_documents::BackendAuth;
use gents_inference::backends::provider::discover_models as discover_backend_models;
use gents_inference::backends::rotation::{set_backend_rotation, BackendRotation};
use gents_protocol::operations::inference::{
    DocumentWrite, InferenceDocument, InferenceOperationError, WriteMode,
};
use gents_service::home_state::resolve_node_did;
use gents_store::config_client::ConfigAccess;
use serde_json::json;

fn failure(verb: &'static str) -> impl Fn(InferenceOperationError) -> InferenceFailure {
    move |error| InferenceFailure::new(error, InferenceDocument::Backend, verb)
}

pub(super) async fn backend_set(args: BackendSetArgs) -> Result<()> {
    let value: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&args.file).with_context(|| format!("reading {}", args.file.display()))?,
    )
    .context("decoding canonical InferenceBackend document")?;
    let (node_did, write) =
        DocumentWrite::from_document(InferenceDocument::Backend, &value, WriteMode::Put)
            .map_err(failure("update"))?;
    let (access, _) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let receipt = gents_inference::operations::write(&access, &node_did, &write)
        .await
        .map_err(failure("update"))?;
    let backend = access
        .transact_readonly("cli.backend.set.read", |txn| {
            let (node_did, backend_id) = (&node_did, &write.id);
            Box::pin(async move {
                gents_documents::load_inference_backend_in_txn(txn, node_did, backend_id)
                    .await?
                    .context("written backend missing")
            })
        })
        .await?;
    crate::print_json(
        &json!({"doc_id":receipt.doc_id,"node_did":backend.node_did,"backend_id":backend.backend_id,"provider_kind":backend.provider_kind.as_str(),"endpoint":backend.endpoint,"created":receipt.created,"changed":receipt.changed}),
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
    if let Some(id) = args.backend_id.as_deref() {
        return discover_stored_backend(&args, id).await;
    }
    let target = resolve_backend_discovery_target(&args)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("building backend discovery client")?;
    anyhow::ensure!(
        !target.provider_kind.is_node_scoped_oauth(),
        "subscription discovery requires --backend-id so the runtime can authorize the selected backend"
    );
    let discovered_models = discover_backend_models(
        &client,
        target.provider_kind,
        &target.endpoint,
        target.api_key.as_deref(),
        None,
    )
    .await?;
    crate::print_json(&json!({
        "backend_id":args.backend_id,"backend_preset":args.backend_preset.map(BackendPresetArg::as_str),
        "provider_kind":target.provider_kind.as_str(),"endpoint":target.endpoint,
        "api_key":target.api_key.as_ref().map(|_|"<redacted>"),"api_key_env_var":target.api_key_env_var,
        "discovered_models":discovered_models,"catalog_written":false,
    }))
}

async fn discover_stored_backend(args: &BackendDiscoverModelsArgs, id: &str) -> Result<()> {
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
    let (backend, models, catalog_written) = match &access {
        ConfigAccess::Graphql(endpoint) => {
            let backend = stored_backend(&access, &owner, id).await?;
            let mut url = reqwest::Url::parse(endpoint.url())?;
            url.set_path(gents_host::http::inference_activity::BACKEND_DISCOVERY_PATH);
            url.set_query(None);
            url.set_fragment(None);
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(30))
                .build()?;
            let response = endpoint
                .authorize(client.post(url))?
                .json(&json!({"node_did":backend.node_did.as_str(), "backend_id":backend.backend_id.as_str()}))
                .send()
                .await?;
            anyhow::ensure!(
                response.status().is_success(),
                "runtime backend discovery failed (HTTP {})",
                response.status()
            );
            let body: serde_json::Value = response.json().await?;
            let models = body
                .get("discovered_models")
                .cloned()
                .context("runtime discovery returned no models")?;
            let written = body
                .get("catalog_written")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            (backend, models, written)
        }
        _ => {
            let discovery = gents_inference::operations::discover(&access, &owner, id)
                .await
                .map_err(failure("discover"))?;
            let written = discovery.answer
                == gents_protocol::operations::inference::DiscoveryAnswer::ContactsProvider;
            (
                discovery.backend,
                serde_json::to_value(discovery.models.unwrap_or_default())?,
                written,
            )
        }
    };
    crate::print_json(
        &json!({"backend_id":id, "provider_kind":backend.provider_kind.as_str(),
        "endpoint":backend.endpoint.as_str(), "api_key":(!backend.auth.uses_node_oauth()).then_some("<redacted>"), "discovered_models":models, "catalog_written":catalog_written}),
    )
}

async fn stored_backend(
    access: &ConfigAccess,
    owner: &str,
    id: &str,
) -> Result<gents_documents::InferenceBackend> {
    access
        .transact_readonly("cli.backend.discovery_target", |txn| {
            Box::pin(async move {
                gents_documents::load_inference_backend_in_txn(txn, owner, id)
                    .await?
                    .context("backend absent in selected owner")
            })
        })
        .await
}

fn resolve_backend_discovery_target(
    args: &BackendDiscoverModelsArgs,
) -> Result<ResolvedBackendConfig> {
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
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_inference::auth::credential::{set_account_enabled, store_sign_in};

    const DID: &str = gents_session::test_support::lineage_fixtures::FIXTURE_DID;
    const CLAUDE: &str = "claude-subscription";

    fn claude_sign_in(who: &str) -> gents_inference::auth::credential::OAuthCredential {
        gents_inference::auth::claude::credential_from_login_tokens(
            DID,
            CLAUDE,
            &gents_inference::auth::claude::ClaudeLoginTokens {
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
            gents_host::test_support::fixture_node_builder()
                .build()
                .await
                .unwrap(),
        );
        gents_store::schema::ensure_runtime_schemas(&node)
            .await
            .unwrap();
        gents_host::test_support::publish_test_node(&node).await;
        let access = ConfigAccess::local_as(
            node,
            ::identity::Did::new(gents_session::test_support::lineage_fixtures::FIXTURE_DID)
                .unwrap(),
        );
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
            gents_documents::write_inference_backend_document(
                &access,
                &backend,
                &gents_inference::resolved::ProviderProfilePolicy,
            )
            .await
            .unwrap();
        }
        access
    }

    async fn stored_auth(access: &ConfigAccess, backend_id: &str) -> BackendAuth {
        access
            .transact_readonly("test.backend.pool.read", |txn| {
                Box::pin(async move {
                    gents_documents::load_inference_backend_in_txn(txn, DID, backend_id)
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
        gents_documents::write_inference_backend_document(
            &access,
            &pinned,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
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
            BackendAuth::Credential { .. }
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
