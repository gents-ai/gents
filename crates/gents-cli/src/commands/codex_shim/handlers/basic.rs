use anyhow::{Context, Result};
use gents::config_client::load_inference_backend_in_txn;
use gents::usage_observation::{usage_for_backend, StoredUsage};
use gents_codex_protocol as codex;
use serde_json::json;

use super::super::bound_behavior::load_bound_model_selection_id_for_state;
use super::super::protocol::{
    account_from_usage, initialize_result, rate_limits_from_usage, send_result,
    send_typed_json_result,
};
use super::super::{Outbound, ShimState};
use super::models::{
    apply_config_writes, available_model_backends, load_bound_agent, model_list_entries,
};
use super::skills::load_skill_metadata;
use crate::config_writes::ConfigAccess;

pub(super) async fn handle_basic_request(
    outbound: &Outbound,
    state: &ShimState,
    request: codex::ClientRequest,
) -> Result<()> {
    match request {
        codex::ClientRequest::Initialize { request_id, .. } => {
            send_typed_json_result::<codex::InitializeResponse>(
                outbound,
                request_id,
                initialize_result(state),
            )
            .await
        }
        codex::ClientRequest::GetAccount { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::GetAccountResponse {
                    account: Some(account_from_usage(session_usage(state).await.as_ref())),
                    requires_openai_auth: false,
                },
            )
            .await
        }
        codex::ClientRequest::GetAccountRateLimits { request_id, .. } => {
            let rate_limits =
                rate_limits_from_usage(session_usage(state).await.as_ref(), chrono::Utc::now());
            send_result(
                outbound,
                request_id,
                codex::GetAccountRateLimitsResponse {
                    rate_limits,
                    rate_limits_by_limit_id: None,
                },
            )
            .await
        }
        codex::ClientRequest::ModelList { request_id, .. } => {
            let agent = load_bound_agent(state)
                .await
                .context("loading bound Agent for ModelList")?;
            let backends = available_model_backends(state)
                .await
                .context("listing available backend models for ModelList")?;
            let entries = model_list_entries(&backends, &agent);
            send_typed_json_result::<codex::ModelListResponse>(
                outbound,
                request_id,
                json!({
                    "data": entries,
                    "nextCursor": null
                }),
            )
            .await
        }
        codex::ClientRequest::ModelProviderCapabilitiesRead { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::ModelProviderCapabilitiesReadResponse {
                    namespace_tools: false,
                    image_generation: false,
                    web_search: false,
                },
            )
            .await
        }
        codex::ClientRequest::ConfigRead { request_id, .. } => {
            let model_id = load_bound_model_selection_id_for_state(
                state.node.as_ref(),
                &state.node_did,
                &state.agent_id,
            )
            .await
            .context("resolving current model selection for ConfigRead")?;
            send_typed_json_result::<codex::ConfigReadResponse>(
                outbound,
                request_id,
                json!({
                    "config": {
                        "model": model_id,
                        "model_provider": "gents",
                        "approval_policy": "never",
                        "sandbox_mode": "danger-full-access"
                    },
                    "origins": {}
                }),
            )
            .await
        }
        codex::ClientRequest::ConfigValueWrite {
            request_id, params, ..
        } => {
            apply_config_writes(
                outbound,
                state,
                request_id,
                vec![(params.key_path, params.value)],
            )
            .await
        }
        codex::ClientRequest::ConfigBatchWrite {
            request_id, params, ..
        } => {
            let writes = params
                .edits
                .into_iter()
                .map(|edit| (edit.key_path, edit.value))
                .collect::<Vec<_>>();
            apply_config_writes(outbound, state, request_id, writes).await
        }
        codex::ClientRequest::ConfigRequirementsRead { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::ConfigRequirementsReadResponse { requirements: None },
            )
            .await
        }
        codex::ClientRequest::ExternalAgentConfigDetect { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::ExternalAgentConfigDetectResponse { items: Vec::new() },
            )
            .await
        }
        codex::ClientRequest::ExternalAgentConfigImport { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::ExternalAgentConfigImportResponse {},
            )
            .await
        }
        codex::ClientRequest::ExperimentalFeatureList { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::ExperimentalFeatureListResponse {
                    data: Vec::new(),
                    next_cursor: None,
                },
            )
            .await
        }
        codex::ClientRequest::PermissionProfileList { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::PermissionProfileListResponse {
                    data: Vec::new(),
                    next_cursor: None,
                },
            )
            .await
        }
        codex::ClientRequest::CollaborationModeList { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::CollaborationModeListResponse { data: Vec::new() },
            )
            .await
        }
        codex::ClientRequest::SkillsList { request_id, .. } => {
            let entry = match load_skill_metadata(state).await {
                Ok(skills) => codex::SkillsListEntry {
                    cwd: state.cwd.clone(),
                    skills,
                    errors: Vec::new(),
                },
                Err(error) => codex::SkillsListEntry {
                    cwd: state.cwd.clone(),
                    skills: Vec::new(),
                    errors: vec![codex::SkillErrorInfo {
                        path: state.cwd.clone(),
                        message: format!("failed to load skills: {error}"),
                    }],
                },
            };
            send_result(
                outbound,
                request_id,
                codex::SkillsListResponse { data: vec![entry] },
            )
            .await
        }
        codex::ClientRequest::HooksList { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::HooksListResponse { data: Vec::new() },
            )
            .await
        }
        codex::ClientRequest::PluginList { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::PluginListResponse {
                    marketplaces: Vec::new(),
                    marketplace_load_errors: Vec::new(),
                    featured_plugin_ids: Vec::new(),
                },
            )
            .await
        }
        codex::ClientRequest::McpServerStatusList { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::ListMcpServerStatusResponse {
                    data: Vec::new(),
                    next_cursor: None,
                },
            )
            .await
        }
        other => unreachable!(
            "non-basic Codex request routed to basic handler: {}",
            other.method()
        ),
    }
}

/// The stored usage of the account the session's backend names, with a
/// plan only for ChatGPT: other providers' plans are not Codex plan types.
/// Never reads the provider: on-demand reads belong to the runtime. Usage
/// gates nothing, so a failed load answers as if nothing were stored.
async fn session_usage(state: &ShimState) -> Option<StoredUsage> {
    let load = async {
        let profile = load_bound_agent(state).await?.inference_profile;
        let node_did = state.node_did.as_ref();
        let backend_id = profile.backend_id.as_str();
        let backend =
            ConfigAccess::transact_local(state.node.as_ref(), None, "codex.usage", |txn| {
                Box::pin(
                    async move { load_inference_backend_in_txn(txn, node_did, backend_id).await },
                )
            })
            .await?;
        let Some(backend) = backend else {
            return Ok(None);
        };
        let mut stored =
            usage_for_backend(&ConfigAccess::Local(state.node.clone()), node_did, &backend).await?;
        if backend.provider_kind != gents::BackendProviderKind::ChatGptCodex {
            if let Some(stored) = stored.as_mut() {
                stored.report.plan = None;
            }
        }
        Ok(stored)
    };
    load.await.unwrap_or_else(|error: anyhow::Error| {
        tracing::warn!(error = %format!("{error:#}"), "reading the session account's usage failed");
        None
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::Utc;
    use gents::config_client::{
        write_inference_backend_document, write_inference_profile_document,
    };
    use gents::defra_node::EmbeddedNode;
    use gents::document_config::{AdvertisedModel, BackendAuth, BackendModelCatalog};
    use gents::oauth_credential::{oauth_credential_id, upsert_oauth_credential, OAuthCredential};
    use gents::usage_observation::account_usage::{UsageReport, UsageSource, UsageWindow};
    use gents::usage_observation::{load_usage, record_usage, UsageAccount};
    use gents::{
        record_model_catalog_in_txn, BackendProviderKind, InferenceBackend, InferenceProfile,
    };
    use serde_json::Value;
    use tokio::sync::{mpsc, Mutex};

    use super::super::super::{CodexSidecar, ShimState};
    use super::*;
    use crate::config_writes::{write_agent_document, ConfigAccess};

    const DID: &str = "did:test:codex-shim-usage";

    fn state(node: Arc<EmbeddedNode>, tempdir: &tempfile::TempDir) -> ShimState {
        ShimState {
            codex_home: tempdir.path().join("codex-home"),
            trace_path: tempdir
                .path()
                .join("codex-home/log/codex-shim-events.jsonl"),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            fs_root: None,
            node,
            background_execution_registry: gents::BackgroundExecutionRegistry::default(),
            graphql: gents::config_client::GraphqlEndpoint::anonymous("http://127.0.0.1/graphql"),
            node_did: Arc::from(DID),
            agent_id: Arc::from("default"),
            id_counter: Arc::new(AtomicU64::new(1)),
            timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(10),
            sidecar: Arc::new(Mutex::new(CodexSidecar::default())),
            auth_token: None,
        }
    }

    /// Binds the session's agent to `backend` through one profile.
    async fn bind(node: &Arc<EmbeddedNode>, backend: InferenceBackend) {
        let access = ConfigAccess::Local(node.clone());
        write_inference_backend_document(&access, &backend)
            .await
            .expect("backend");
        ConfigAccess::transact_local(node.as_ref(), None, "codex.usage.test.catalog", |txn| {
            let backend = &backend;
            Box::pin(async move {
                record_model_catalog_in_txn(
                    txn,
                    backend,
                    BackendModelCatalog {
                        node_did: backend.catalog_scope().map(str::to_owned),
                        observed_at: Utc::now().to_rfc3339(),
                        models: vec![AdvertisedModel {
                            model_name: "model-a".into(),
                            display_name: None,
                            context_window: None,
                            max_context_window: None,
                            max_output_tokens: None,
                            reasoning_efforts: None,
                        }],
                    },
                )
                .await
            })
        })
        .await
        .expect("catalog");
        write_inference_profile_document(
            &access,
            &InferenceProfile {
                node_did: DID.into(),
                profile_id: "profile-a".into(),
                display_name: None,
                description: None,
                backend_id: backend.backend_id.clone(),
                model_name: "model-a".into(),
                reasoning_effort: None,
                context_window: None,
                max_output_tokens: None,
                sampling_id: None,
                execution_id: None,
                tags: Vec::new(),
            },
        )
        .await
        .expect("profile");
        write_agent_document(
            &access,
            &gents::AgentDocument {
                agent_id: "default".into(),
                node_did: DID.into(),
                display_name: None,
                description: None,
                context_id: None,
                inference_profile_id: "profile-a".into(),
                enabled: true,
                tags: Vec::new(),
                created_at: None,
            },
        )
        .await
        .expect("agent");
    }

    fn backend(provider_kind: BackendProviderKind, auth: BackendAuth) -> InferenceBackend {
        InferenceBackend {
            node_did: DID.into(),
            backend_id: "backend-usage-a".into(),
            name: "backend-usage-a".into(),
            provider_kind,
            openai_wire_api: None,
            endpoint: "http://127.0.0.1:9/v1".into(),
            auth,
            connect_timeout_secs: None,
            discovery_timeout_secs: None,
            max_concurrent: None,
            max_queue_depth: None,
            enabled: true,
            tags: Vec::new(),
        }
    }

    async fn rate_limits(state: &ShimState) -> Value {
        let (outbound, mut received) = mpsc::unbounded_channel();
        let request: codex::ClientRequest =
            serde_json::from_value(json!({ "id": 1, "method": "account/rateLimits/read" }))
                .expect("request");
        handle_basic_request(&outbound, state, request)
            .await
            .expect("handled");
        let response: Value =
            serde_json::from_str(&received.recv().await.expect("response")).expect("json");
        response["result"]["rateLimits"].clone()
    }

    #[tokio::test]
    async fn shim_usage_rate_limits_come_from_the_session_account() {
        let tempdir = tempfile::tempdir().unwrap();
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let provider = "chatgpt-codex";
        upsert_oauth_credential(
            &node,
            &OAuthCredential {
                doc_id: None,
                credential_id: oauth_credential_id(DID, provider),
                node_did: DID.into(),
                provider: provider.into(),
                access_token: "access-TEST".into(),
                refresh_token: "refresh-TEST".into(),
                id_token: None,
                account_id: None,
                chatgpt_plan_type: Some("plus".into()),
                is_fedramp: false,
                access_token_expires_at: Utc::now() + chrono::Duration::hours(1),
                last_refresh: None,
                enabled: true,
                account_ref: None,
                connected_at: None,
                provider_account_key: Some("acct-key-a".into()),
                label: None,
            },
        )
        .await
        .unwrap();
        bind(
            &node,
            backend(
                BackendProviderKind::ChatGptCodex,
                BackendAuth::NodeOAuth { account_ref: None },
            ),
        )
        .await;
        let account = UsageAccount::Credential {
            doc_id: None,
            node_did: DID.into(),
            provider: provider.into(),
            account_ref: None,
        };
        record_usage(
            &node,
            &account,
            UsageReport {
                windows: vec![UsageWindow {
                    label: "primary".into(),
                    window_minutes: Some(300),
                    used_pct: 12.0,
                    resets_at: None,
                    source: UsageSource::Header,
                    observed_at: Utc::now(),
                }],
                ..UsageReport::default()
            },
        )
        .await
        .unwrap();
        let state = state(node.clone(), &tempdir);

        let limits = rate_limits(&state).await;

        assert_eq!(limits["primary"]["usedPercent"], json!(12));
        assert_eq!(limits["primary"]["windowDurationMins"], json!(300));
        assert_eq!(limits["planType"], json!("plus"));
        let stored = load_usage(&ConfigAccess::Local(node), &account)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.read_at, None, "the shim never reads upstream");
    }

    fn credential(provider: &str, plan: Option<&str>) -> OAuthCredential {
        OAuthCredential {
            doc_id: None,
            credential_id: oauth_credential_id(DID, provider),
            node_did: DID.into(),
            provider: provider.into(),
            access_token: "access-TEST".into(),
            refresh_token: "refresh-TEST".into(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: plan.map(str::to_owned),
            is_fedramp: false,
            access_token_expires_at: Utc::now() + chrono::Duration::hours(1),
            last_refresh: None,
            enabled: true,
            account_ref: None,
            connected_at: None,
            provider_account_key: Some("acct-key-a".into()),
            label: None,
        }
    }

    #[tokio::test]
    async fn shim_usage_non_codex_account_sends_no_plan() {
        let tempdir = tempfile::tempdir().unwrap();
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();
        upsert_oauth_credential(&node, &credential("xai-oauth", None))
            .await
            .unwrap();
        bind(
            &node,
            backend(
                BackendProviderKind::XaiGrokOAuth,
                BackendAuth::NodeOAuth { account_ref: None },
            ),
        )
        .await;
        record_usage(
            &node,
            &UsageAccount::Credential {
                doc_id: None,
                node_did: DID.into(),
                provider: "xai-oauth".into(),
                account_ref: None,
            },
            UsageReport {
                windows: vec![UsageWindow {
                    label: "primary".into(),
                    window_minutes: Some(300),
                    used_pct: 30.0,
                    resets_at: None,
                    source: UsageSource::Header,
                    observed_at: Utc::now(),
                }],
                plan: Some(gents::usage_observation::account_usage::UsagePlan {
                    name: "tier-a".into(),
                    observed_at: Utc::now(),
                }),
                ..UsageReport::default()
            },
        )
        .await
        .unwrap();
        let state = state(node, &tempdir);

        let limits = rate_limits(&state).await;
        assert_eq!(limits["planType"], Value::Null);
        assert_eq!(limits["primary"]["usedPercent"], json!(30));
    }

    #[tokio::test]
    async fn shim_usage_failed_load_answers_empty() {
        let tempdir = tempfile::tempdir().unwrap();
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();
        // No agent is bound, so loading the session's account fails.
        let state = state(node, &tempdir);
        let (outbound, mut received) = mpsc::unbounded_channel();
        let request: codex::ClientRequest =
            serde_json::from_value(json!({ "id": 1, "method": "account/rateLimits/read" }))
                .expect("request");

        let handled = handle_basic_request(&outbound, &state, request).await;

        assert!(handled.is_ok(), "{handled:?}");
        let response: Value =
            serde_json::from_str(&received.recv().await.expect("response")).expect("json");
        assert_eq!(
            response["result"]["rateLimits"],
            serde_json::to_value(super::super::super::protocol::empty_rate_limits()).unwrap()
        );
    }

    #[tokio::test]
    async fn account_read_comes_from_the_session_account() {
        let tempdir = tempfile::tempdir().unwrap();
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();
        upsert_oauth_credential(&node, &credential("chatgpt-codex", Some("plus")))
            .await
            .unwrap();
        bind(
            &node,
            backend(
                BackendProviderKind::ChatGptCodex,
                BackendAuth::NodeOAuth { account_ref: None },
            ),
        )
        .await;
        let state = state(node, &tempdir);
        let (outbound, mut received) = mpsc::unbounded_channel();
        let request: codex::ClientRequest =
            serde_json::from_value(json!({ "id": 1, "method": "account/read", "params": {} }))
                .expect("request");

        handle_basic_request(&outbound, &state, request)
            .await
            .expect("handled");

        let response = received.recv().await.expect("response");
        assert!(!response.contains("TEST"), "{response}");
        let response: Value = serde_json::from_str(&response).expect("json");
        assert_eq!(
            response["result"],
            json!({
                "account": { "type": "chatgpt", "email": "", "planType": "plus" },
                "requiresOpenaiAuth": false,
            })
        );
    }

    #[tokio::test]
    async fn shim_usage_non_codex_backend_answers_as_before() {
        let tempdir = tempfile::tempdir().unwrap();
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        gents::ensure_runtime_schemas(&node).await.unwrap();
        bind(
            &node,
            backend(
                BackendProviderKind::OpenAiCompatible,
                BackendAuth::Unauthenticated,
            ),
        )
        .await;
        let state = state(node, &tempdir);

        assert_eq!(
            rate_limits(&state).await,
            serde_json::to_value(super::super::super::protocol::empty_rate_limits()).unwrap()
        );
    }
}
