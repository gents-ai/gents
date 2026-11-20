use std::sync::Arc;

use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use gents_protocol::{
    document_query::{fields::agent_session_goal_control as fields, Filter, Query, Selection},
    goal_control::{ApplyGoalControl, SessionGoalControl},
};
use gents_store::{
    config_client::{client_authentication::ClientAuthorization, ConfigAccess, ConfigClientError},
    identity::NodeIdentity,
};

#[derive(Debug, thiserror::Error)]
pub enum GoalApplicationError {
    #[error("this client cannot apply the selected Goal command")]
    Refused,
    #[error(fmt = gents_protocol::error_chain::held)]
    Access(#[from] ConfigClientError),
    #[error(fmt = gents_protocol::error_chain::held)]
    View(#[from] gents_store::views::ViewError),
    #[error("applying the signed Goal command")]
    Application(#[source] anyhow::Error),
}

pub async fn apply(
    node: &Arc<defra_node::Node>,
    runtime: &Arc<dyn NodeIdentity>,
    authorization: ClientAuthorization,
    request: &ApplyGoalControl,
) -> Result<(), GoalApplicationError> {
    if request.node_did != runtime.did() || node.node_identity_did() != Some(runtime.did()) {
        return Err(GoalApplicationError::Refused);
    }
    let actor = authorization.actor().clone();
    let access = ConfigAccess::local_as(node.clone(), actor.clone())
        .with_client_authorization(authorization.clone())?;
    let response = access
        .read(
            &Query::select(Selection::doc_id().field(fields::INTENT_JSON))
                .filter(Filter::eq(fields::DOC_ID, &request.command_doc_id))
                .limit(2)
                .render(),
        )
        .await?;
    let Some([row]) = response["data"]["AgentSessionGoalControl"]
        .as_array()
        .map(Vec::as_slice)
    else {
        return Err(GoalApplicationError::Refused);
    };
    let command: SessionGoalControl = row["intent_json"]
        .as_str()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .ok_or(GoalApplicationError::Refused)?;
    if command.requester_did != actor.as_str()
        || command.node_did != request.node_did
        || command.agent_did != request.agent_did
    {
        return Err(GoalApplicationError::Refused);
    }
    let agents = access
        .read_view(
            &gents_documents::views::agents::AgentList::new(&request.node_did)
                .agent_did(&request.agent_did)
                .limit(2),
        )
        .await?;
    let [agent] = agents.as_slice() else {
        return Err(GoalApplicationError::Refused);
    };
    let binding = gents_protocol::agent_identity::AgentIdentityBinding {
        agent_did: agent.agent.agent_did.clone(),
        creation_nonce: agent.agent.creation_nonce.clone(),
    };
    let agent = gents_store::identity::resolve_agent_identity(runtime.as_ref(), &binding)
        .map_err(|error| GoalApplicationError::Application(error.into()))?;
    super::session_goal_control::process_command(
        node,
        runtime,
        &request.command_doc_id,
        agent.as_ref(),
        Some(authorization),
    )
    .await
    .map_err(GoalApplicationError::Application)?;
    Ok(())
}

pub async fn apply_http(
    node: &Arc<defra_node::Node>,
    runtime: &Arc<dyn NodeIdentity>,
    actor: &defra_http::ExtractIdentity,
    headers: &HeaderMap,
    request: ApplyGoalControl,
) -> Response {
    let Ok(authorization) = ClientAuthorization::from_http(actor, headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match apply(node, runtime, authorization, &request).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(GoalApplicationError::Refused) => StatusCode::FORBIDDEN.into_response(),
        Err(GoalApplicationError::Access(error)) if error.is_permission_denied() => {
            StatusCode::FORBIDDEN.into_response()
        }
        Err(GoalApplicationError::View(gents_store::views::ViewError::Execute(error)))
            if error.is_permission_denied() =>
        {
            StatusCode::FORBIDDEN.into_response()
        }
        Err(error) => {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "Goal command application failed");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

#[cfg(test)]
mod access_classification;
