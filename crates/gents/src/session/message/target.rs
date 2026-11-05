use anyhow::Result;
use defra_node::Node as EmbeddedNode;

use std::sync::Arc;

use crate::request::lifecycle::SessionMessageTarget;
use crate::session::origin::SessionScope;
use crate::store::config_client::ConfigAccess;
use crate::tools::surface::AgentToolConfig;

pub(crate) async fn target_agent_exists(
    access: &ConfigAccess,
    node_did: &str,
    agent_did: &str,
) -> Result<bool> {
    let node_did = node_did.to_owned();
    let agent_did = agent_did.to_owned();
    access
        .transact_readonly("session_message.target_agent", move |txn| {
            let node_did = node_did.clone();
            let agent_did = agent_did.clone();
            Box::pin(async move {
                Ok(
                    crate::store::config_client::read_desired_state_document_in_txn(
                        txn,
                        crate::Collection::Agent,
                        &node_did,
                        &agent_did,
                    )
                    .await?
                    .is_some(),
                )
            })
        })
        .await
}

pub(crate) async fn load_caller_session_tools(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_did: &str,
) -> Result<AgentToolConfig> {
    let (_, tools, targets) =
        crate::request::admission::load_request_context(node, node_did, agent_did).await?;
    tools
        .map(|tools| AgentToolConfig::from_document_with_targets(&tools, &targets))
        .transpose()
        .map(Option::unwrap_or_default)
}

pub(crate) async fn resolve_send_target(
    node: &Arc<EmbeddedNode>,
    caller_node_did: &str,
    caller_agent_did: &str,
    tools: &AgentToolConfig,
    session_id: &str,
) -> Result<Option<SessionMessageTarget>> {
    let Some(session) = crate::session::origin::load_session(
        &ConfigAccess::local_as(node.clone(), identity::Did::new(caller_agent_did)?),
        session_id,
        Some(caller_agent_did),
    )
    .await?
    else {
        return Ok(None);
    };
    Ok(
        session_target_allowed(&session, caller_node_did, caller_agent_did, tools).then_some(
            SessionMessageTarget {
                node_did: session.node_did,
                agent_did: session.agent_did,
                session_id: session.session_id,
            },
        ),
    )
}

pub(super) fn session_target_allowed(
    session: &gents_protocol::session::AgentSession,
    caller_node_did: &str,
    caller_agent_did: &str,
    tools: &AgentToolConfig,
) -> bool {
    session.requester_did.as_deref() == Some(caller_agent_did)
        && (session.node_did == caller_node_did
            || (session
                .provenance
                .as_ref()
                .is_some_and(|origin| origin.parent_request_doc_id.is_some())
                && tools.allows(&session.node_did, &session.agent_did)))
}

pub(crate) fn target_scope(caller_agent_did: &str, target: &SessionMessageTarget) -> SessionScope {
    SessionScope {
        node_did: target.node_did.clone(),
        session_id: target.session_id.clone(),
        requester_did: Some(caller_agent_did.to_owned()),
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
                "agent_did": target.agent_did,
                "description": target.description,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "ok": true, "agents": agents }))
}
