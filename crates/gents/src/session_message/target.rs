use anyhow::Result;
use defra_node::EmbeddedNode;

use std::sync::Arc;

use crate::config_client::ConfigAccess;
use crate::lifecycle::SessionMessageTarget;
use crate::session_origin::SessionScope;
use crate::tool_surface::AgentToolConfig;

pub(crate) async fn load_caller_session_tools(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
) -> Result<AgentToolConfig> {
    let (_, tools, targets) =
        crate::request_admission::load_request_context(node, node_did, agent_id).await?;
    tools
        .map(|tools| AgentToolConfig::from_document_with_targets(&tools, &targets))
        .transpose()
        .map(Option::unwrap_or_default)
}

pub(crate) async fn resolve_send_target(
    node: &Arc<EmbeddedNode>,
    caller_node_did: &str,
    tools: &AgentToolConfig,
    session_id: &str,
) -> Result<Option<SessionMessageTarget>> {
    let Some(session) = crate::session_origin::load_session(
        &ConfigAccess::local(node.clone()),
        session_id,
        Some(caller_node_did),
    )
    .await?
    else {
        return Ok(None);
    };
    Ok(
        session_target_allowed(&session, caller_node_did, tools).then_some(SessionMessageTarget {
            node_did: session.node_did,
            agent_id: session.agent_id,
            session_id: session.session_id,
        }),
    )
}

pub(super) fn session_target_allowed(
    session: &gents_protocol::session::AgentSession,
    caller_node_did: &str,
    tools: &AgentToolConfig,
) -> bool {
    session.requester_did.as_deref() == Some(caller_node_did)
        && (session.node_did == caller_node_did
            || (session
                .provenance
                .as_ref()
                .is_some_and(|origin| origin.parent_request_doc_id.is_some())
                && tools.allows(&session.node_did, &session.agent_id)))
}

pub(crate) fn target_scope(caller_node_did: &str, target: &SessionMessageTarget) -> SessionScope {
    SessionScope {
        node_did: target.node_did.clone(),
        session_id: target.session_id.clone(),
        requester_did: Some(caller_node_did.to_owned()),
    }
}

pub(crate) async fn agent_list(
    _node: &Arc<EmbeddedNode>,
    _caller: &crate::AgentRequest,
    tools: &AgentToolConfig,
) -> Result<serde_json::Value> {
    let agents = tools
        .targets
        .iter()
        .map(|target| {
            serde_json::json!({
                "agent": target.name,
                "node_did": target.target_node_did,
                "agent_id": target.agent_id,
                "description": target.description,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "ok": true, "agents": agents }))
}
