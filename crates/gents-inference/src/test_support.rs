pub mod embedded;
pub use embedded::default_agent;

use gents_documents::test_support::{ensure_test_node, set_test_default_agent_as};
use gents_store::test_support::held_runtime_identity;

/// A request with fixed identity and no execution state, for code that reads
/// only what a request names.
pub fn agent_request(request_id: &str) -> gents_protocol::row::AgentRequest {
    gents_protocol::row::AgentRequest {
        retry_parent_request_doc_id: None,
        purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        doc_id: format!("doc-{request_id}"),
        request_id: request_id.to_string(),
        node_did: "did:test:test".to_string(),
        requester_did: None,
        agent_did: "default".to_string(),
        session_id: format!("session-{request_id}"),
        content: "hello".to_string(),
        max_total_tokens: None,
        input: Default::default(),
        execution_origin: None,
        created_at: "2026-04-15T00:00:00Z".to_string(),
        deadline: None,
        execution_generation: None,
        execution_lease_expires_at: None,
        execution_lease_secs: None,
        request_hop: 0,
        interrupt_generation: None,
        caused_by_parent_request_id: None,
        caused_by_parent_request_doc_id: None,
        caused_by_parent_tool_call_id: None,
        caused_by_parent_tool_call_doc_id: None,
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
        caused_by_source_doc_id: None,
        caused_by_correlation: None,
        caused_by_trigger_context: None,
        workspace_id: None,
        workspace_authority: None,
        workspace_owner_node_did: None,
        workspace_seal_hash: None,
        executed_model_name: None,
        executed_reasoning_effort: None,
        executed_service_tier: None,
        executed_inference_profile_id: None,
        executed_runtime_generation: None,
    }
}

pub async fn ensure_test_default_agent(
    node: &std::sync::Arc<defra_node::Node>,
    owner: &str,
) -> gents_protocol::agent_identity::AgentIdentityBinding {
    ensure_test_node(node, owner).await;
    if let Some(did) = gents_documents::load_default_agent_did(node, owner)
        .await
        .unwrap()
    {
        let agent = gents_documents::load_agent(node, owner, &did)
            .await
            .unwrap()
            .expect("fixture default agent exists");
        return gents_protocol::agent_identity::AgentIdentityBinding {
            agent_did: agent.agent_did,
            creation_nonce: agent.creation_nonce,
        };
    }
    let binding = install_test_agent(node, owner, "Default fixture agent").await;
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(owner).unwrap(),
    );
    set_test_default_agent_as(&access, owner, &binding.agent_did).await;
    binding
}

pub async fn install_test_agent(
    node: &std::sync::Arc<defra_node::Node>,
    owner: &str,
    display_name: &str,
) -> gents_protocol::agent_identity::AgentIdentityBinding {
    let node_did = node
        .node_identity_did()
        .expect("fixture node has an identity");
    let root = held_runtime_identity(node_did);
    let binding = gents_store::identity::provision_agent_identity(&root).unwrap();
    install_test_agent_binding(node, owner, display_name, &binding).await;
    binding
}

pub async fn install_test_agent_binding(
    node: &std::sync::Arc<defra_node::Node>,
    owner: &str,
    display_name: &str,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
) {
    ensure_test_node(node, owner).await;
    let actor = node
        .node_identity_did()
        .expect("fixture node has an identity");
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(actor).unwrap(),
    );
    install_test_agent_binding_with_access(owner, display_name, binding, &access).await;
}

pub async fn install_test_agent_binding_with_access(
    owner: &str,
    display_name: &str,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    access: &gents_store::config_client::ConfigAccess,
) {
    gents_documents::test_support::install_test_agent_as(
        access,
        owner,
        display_name,
        binding,
        &crate::resolved::ProviderProfilePolicy,
    )
    .await;
    embedded::mark_backend_healthy(access, owner, binding).await;
}

/// A static-configuration agent over `identity`, with the owner's defaults.
/// `binding` fixes the agent identity; absent, one is provisioned.
pub fn resolved_agent(
    identity: std::sync::Arc<dyn gents_store::identity::NodeIdentity>,
    binding: Option<gents_protocol::agent_identity::AgentIdentityBinding>,
) -> crate::resolved::ResolvedAgent {
    use crate::resolved::{
        MaxTurnsProvenance, ResolvedAgent, ResolvedConfiguration, DEFAULT_CONTEXT_WINDOW,
        DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MAX_TURNS, DEFAULT_MODEL_NAME,
        DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS, DEFAULT_STREAM_BATCH_MS, DEFAULT_TITLE_TIMEOUT_SECS,
    };
    use gents_documents::inference_defaults::{
        DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
    };
    use std::time::Duration;
    let binding = binding.unwrap_or_else(|| {
        gents_store::identity::provision_agent_identity(identity.as_ref()).unwrap()
    });
    let agent_identity =
        gents_store::identity::resolve_agent_identity(identity.as_ref(), &binding).unwrap();
    let node = std::sync::Arc::new(gents_store::identity::RuntimeNode {
        node_did: identity.did().to_string(),
        identity,
        default_agent_did: binding.agent_did.clone(),
        display_name: None,
        enabled: true,
    });
    ResolvedAgent {
        configuration: ResolvedConfiguration::Static,
        profile_id: None,
        agent_did: binding.agent_did,
        agent_identity,
        node,
        backend_id: None,
        backend_provider_kind: crate::backends::provider::BackendProviderKind::OpenAiCompatible,
        openai_wire_api: gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
        backend_endpoint: "http://127.0.0.1:1/v1".to_string(),
        backend_auth: gents_documents::BackendAuth::Unauthenticated,
        claude_platform: None,
        client_version: None,
        model_name: DEFAULT_MODEL_NAME.to_string(),
        service_tier: None,
        resolved_reasoning_efforts: None,
        context_window: DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: DEFAULT_MAX_TURNS,
        max_turns_provenance: MaxTurnsProvenance::Default,
        system_prompt: String::new(),
        compaction: None,
        compaction_inference: None,
        max_total_tokens: None,
        max_invalid_tool_calls: gents_loop::loop_stream::DEFAULT_MAX_INVALID_TOOL_CALLS,
        repeated_tool_failure_limit: gents_loop::loop_stream::DEFAULT_REPEATED_TOOL_FAILURE_LIMIT,
        max_request_retries: gents_documents::inference_defaults::DEFAULT_REQUEST_MAX_RETRIES,
        stream_batch_ms: DEFAULT_STREAM_BATCH_MS,
        stream_liveness_timeout: Duration::from_secs(DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS),
        provider_idle_timeout: Duration::from_secs(DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS),
        title_timeout: Duration::from_secs(DEFAULT_TITLE_TIMEOUT_SECS),
        deadline_duration: Duration::from_secs(DEFAULT_DEADLINE_DURATION_SECS),
        completion_retry: Default::default(),
        sampling: Default::default(),
        skills: Vec::new(),
        oauth_pool: Vec::new(),
    }
}

pub mod document_fixtures;
pub mod scripted_backend;
