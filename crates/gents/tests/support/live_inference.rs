//! Shared backend binding, runtime boot and durable observation for evals.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gents::defra_node::EmbeddedNode;
use gents::document_config::{
    AgentBehavior, AgentPrincipal, BackendAuth, InferenceBackend, InferenceProfile,
};
use gents::graphql::escape_graphql_string;
use gents::{
    default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    ensure_agent_principal, AgentIdentity, BackendProviderKind, Collection, DocumentRuntimeOptions,
    Gents, OpenAiWireApi, ToolCeiling,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;

use crate::support::interrupt::{wait_for_runtime_ready, BootedAgent};
use crate::support::{first_optional_row, TestDb};

pub fn d4f_enabled() -> bool {
    std::env::var("GENTS_D4F_LIVE").as_deref() == Ok("1")
}

pub const D4F_BACKEND_ID: &str = "backend-d4f-live";
pub const OPENROUTER_BACKEND_ID: &str = "backend-openrouter-live";

/// Live backend endpoint/model, overridable for workstation deployments.
fn d4f_endpoint() -> String {
    std::env::var("GENTS_D4F_ENDPOINT")
        .unwrap_or_else(|_| "http://workstation-1:8000/v1".to_string())
}

fn d4f_model() -> String {
    std::env::var("GENTS_D4F_MODEL").unwrap_or_else(|_| "GLM-5.3-Flash-NVFP4".to_string())
}

pub async fn bind_d4f_backend(
    node: &EmbeddedNode,
    identity: &dyn AgentIdentity,
) -> (String, String) {
    bind_d4f_backend_for_model(node, identity, &d4f_model()).await
}

/// Bind one isolated live-test principal to an explicit model.
///
/// Eval harnesses use this entry point instead of mutating process environment
/// so multiple model trials remain deterministic and can eventually run in
/// parallel safely.
pub async fn bind_d4f_backend_for_model(
    node: &EmbeddedNode,
    identity: &dyn AgentIdentity,
    model: &str,
) -> (String, String) {
    bind_live_backend_for_model(node, identity, model, d4f_backend).await
}

/// Bind one isolated live-test principal to OpenRouter without copying the
/// operator's credential into DefraDB. Runtime auth resolves the key from the
/// named host environment variable at the provider boundary.
pub async fn bind_openrouter_backend_for_model(
    node: &EmbeddedNode,
    identity: &dyn AgentIdentity,
    model: &str,
) -> (String, String) {
    bind_live_backend_for_model(node, identity, model, openrouter_backend).await
}

async fn bind_live_backend_for_model(
    node: &EmbeddedNode,
    identity: &dyn AgentIdentity,
    model: &str,
    backend: fn(&str) -> InferenceBackend,
) -> (String, String) {
    let agent_did = identity.did().to_string();
    let mut principal = ensure_agent_principal(node, &agent_did)
        .await
        .expect("ensure principal");
    let behavior_id = default_behavior_id_for_agent(&agent_did);
    let profile_id = default_inference_profile_id_for_behavior(&behavior_id);
    principal.default_behavior_id = Some(behavior_id.clone());
    let backend = backend(&agent_did);
    let profile = InferenceProfile {
        agent_did: agent_did.clone(),
        profile_id: profile_id.clone(),
        backend_id: backend.backend_id.clone(),
        model_name: model.to_owned(),
        ..Default::default()
    };
    let behavior = AgentBehavior {
        behavior_id: behavior_id.clone(),
        agent_did: agent_did.clone(),
        display_name: Some("Live default behavior".to_string()),
        description: None,
        context_id: None,
        inference_profile_id: profile_id,
        enabled: true,
        tags: Vec::new(),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };

    apply_live_backend_documents(node, principal, backend, profile, behavior).await;

    debug_assert_eq!(behavior_id, default_behavior_id_for_agent(&agent_did));
    (agent_did, behavior_id)
}

fn d4f_backend(agent_did: &str) -> InferenceBackend {
    InferenceBackend {
        agent_did: agent_did.to_string(),
        backend_id: D4F_BACKEND_ID.to_string(),
        name: D4F_BACKEND_ID.to_string(),
        provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: Some(OpenAiWireApi::ChatCompletions),
        endpoint: d4f_endpoint(),
        auth: BackendAuth::Unauthenticated,
        connect_timeout_secs: None,
        discovery_timeout_secs: None,
        max_concurrent: Some(4),
        max_queue_depth: Some(100),
        enabled: true,
        tags: Vec::new(),
    }
}

fn openrouter_backend(agent_did: &str) -> InferenceBackend {
    InferenceBackend {
        agent_did: agent_did.to_string(),
        backend_id: OPENROUTER_BACKEND_ID.to_string(),
        name: "OpenRouter live eval".to_string(),
        provider_kind: BackendProviderKind::OpenRouter,
        openai_wire_api: Some(OpenAiWireApi::ChatCompletions),
        endpoint: gents::inference_setup::OPENROUTER_ENDPOINT.to_string(),
        auth: BackendAuth::Environment {
            variable: "OPENROUTER_API_KEY".to_string(),
        },
        connect_timeout_secs: None,
        discovery_timeout_secs: None,
        max_concurrent: Some(4),
        max_queue_depth: Some(100),
        enabled: true,
        tags: Vec::new(),
    }
}

async fn apply_live_backend_documents(
    node: &EmbeddedNode,
    principal: AgentPrincipal,
    backend: InferenceBackend,
    profile: InferenceProfile,
    behavior: AgentBehavior,
) {
    use gents::config_client::{
        apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    let plan = DesiredStateApplyPlan::new(
        [
            (Collection::AgentPrincipal, serde_json::to_value(principal)),
            (Collection::InferenceBackend, serde_json::to_value(backend)),
            (Collection::InferenceProfile, serde_json::to_value(profile)),
            (Collection::AgentBehavior, serde_json::to_value(behavior)),
        ]
        .into_iter()
        .map(|(collection, value)| {
            let value = value.expect("serialize live configuration document");
            DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            }
        })
        .collect(),
    )
    .expect("build live backend plan");
    gents::ConfigAccess::transact_local(node, None, "test.bind_live_backend", |txn| {
        let plan = &plan;
        Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
    })
    .await
    .expect("upsert live backend");
}

pub async fn boot_d4f_agent(db: &TestDb, identity: Arc<dyn AgentIdentity>) -> Result<BootedAgent> {
    boot_d4f_agent_with_ceiling(db, identity, ToolCeiling::meta_only()).await
}

pub async fn boot_d4f_agent_with_ceiling(
    db: &TestDb,
    identity: Arc<dyn AgentIdentity>,
    tool_ceiling: ToolCeiling,
) -> Result<BootedAgent> {
    Ok(boot_d4f_agent_with_options(
        db,
        identity,
        DocumentRuntimeOptions {
            tool_ceiling,
            ..Default::default()
        },
    )
    .await?
    .0)
}

pub async fn boot_d4f_agent_with_options(
    db: &TestDb,
    identity: Arc<dyn AgentIdentity>,
    options: DocumentRuntimeOptions,
) -> Result<(BootedAgent, Gents)> {
    let agent = Gents::from_default_behavior_documents(db.node.clone(), identity, options).await?;
    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.clone().run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    Ok((BootedAgent::new(shutdown_tx, handle, agent_did), agent))
}

pub async fn assert_d4f_reachable() {
    let url = format!("{}/models", d4f_endpoint().trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client");
    let resp = tokio::time::timeout(Duration::from_secs(20), client.get(&url).send()).await;
    match resp {
        Ok(Ok(r)) if r.status().is_success() => {}
        Ok(Ok(r)) => panic!("d4f endpoint {url} returned status {}", r.status()),
        Ok(Err(e)) => panic!("d4f endpoint {url} unreachable: {e}"),
        Err(_) => panic!("d4f endpoint {url} timed out (not reachable)"),
    }
}

fn is_terminal(state: &str) -> bool {
    RequestLifecycleState::is_terminal_str(Some(state))
}

async fn fetch_request_lifecycle(node: &EmbeddedNode, request_id: &str) -> Option<String> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                lifecycle_state
            }}
        }}"#
    );
    #[derive(Deserialize)]
    struct Row {
        lifecycle_state: Option<String>,
    }
    let resp = node.execute(&query).await;
    first_optional_row::<Row>(&resp, "AgentRequest").and_then(|r| r.lifecycle_state)
}

pub async fn wait_for_request_terminal(
    node: &EmbeddedNode,
    request_id: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = String::from("<none>");
    loop {
        if let Some(state) = fetch_request_lifecycle(node, request_id).await {
            last = state.clone();
            if is_terminal(&state) {
                return state;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for request {request_id} to terminalize; last={last}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn fetch_assistant_answer(node: &EmbeddedNode, request_id: &str) -> String {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentResponse(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                content
                session_id
            }}
        }}"#
    );
    #[derive(Deserialize)]
    struct RespRow {
        content: Option<String>,
        session_id: Option<String>,
    }
    let resp = node.execute(&query).await;
    let row = first_optional_row::<RespRow>(&resp, "AgentResponse");
    if let Some(row) = &row {
        if let Some(content) = row.content.as_deref() {
            if !content.trim().is_empty() {
                return gents_protocol::transcript::present_persisted_message("assistant", content)
                    .body_markdown;
            }
        }
    }
    let session_id = match row.and_then(|r| r.session_id) {
        Some(s) if !s.is_empty() => s,
        _ => return String::new(),
    };
    let escaped_session = escape_graphql_string(&session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped_session}" }}, role: {{ _eq: "assistant" }} }},
                order: {{ sequence: DESC }},
                limit: 1
            ) {{ content }}
        }}"#
    );
    #[derive(Deserialize)]
    struct MsgRow {
        content: String,
    }
    let resp = node.execute(&query).await;
    first_optional_row::<MsgRow>(&resp, "AgentMessage")
        .map(|m| {
            gents_protocol::transcript::present_persisted_message("assistant", &m.content)
                .body_markdown
        })
        .unwrap_or_default()
}

pub async fn wait_for_assistant_answer(
    node: &EmbeddedNode,
    request_id: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let answer = fetch_assistant_answer(node, request_id).await;
        if !answer.trim().is_empty() {
            return answer;
        }
        if tokio::time::Instant::now() >= deadline {
            return answer;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
