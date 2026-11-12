use anyhow::Result;
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::request_input::RequestInput;
use gents_session::request::lifecycle::TriggerLineage;
use gents_store::config_client::ConfigAccess;

use crate::client::store::ClientStore;

#[cfg(test)]
use super::super::graphql::normalize_required;
use prepare::prepare_request_submission;

/// The request the runtime holds for a send. Replica observation may still be
/// pending; callers must not resend under a new identity when a subsequent
/// refresh fails.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SubmittedRequest {
    pub request_id: String,
    pub session_id: String,
    pub node_did: String,
    pub agent_did: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SubmitRequestOptions {
    pub input: RequestInput,
    pub trigger_lineage: TriggerLineage,
    pub retry_key: Option<String>,
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub async fn submit_goal_backed_request(
    store: &ClientStore,
    access: &ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester_did: &str,
    signer: &dyn gents_store::identity::NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    content: &str,
    agent_did: Option<&str>,
    mut options: SubmitRequestOptions,
    objective: &str,
    token_budget: Option<i64>,
) -> Result<SubmittedRequest> {
    if options.retry_key.is_none() {
        options.retry_key = Some(format!(
            "desktop-goal-request:{}",
            normalize_required("session_id", session_id)?
        ));
    }
    options
        .input
        .goal_assignment
        .get_or_insert(gents_protocol::request_input::GoalAssignmentInput {
            goal_id: gents_protocol::goal::deterministic_goal_id(node_did, session_id),
            root_request_doc_id: None,
        });
    options.input = gents_session::request::lifecycle::prepare_user_message_input(
        access,
        node_did,
        session_id,
        options.input,
        gents_protocol::request_input::QueueDelivery::Queue,
    )
    .await?;
    let (result, create) = prepare_request_submission(
        access,
        store,
        session_id,
        node_did,
        requester_did,
        signer,
        admission,
        content,
        agent_did,
        options,
        None,
    )
    .await?;
    let input = gents_protocol::operations::goal::GoalStartInput {
        session_id: session_id.to_owned(),
        objective: objective.to_owned(),
        token_budget,
    };
    gents_automation::operations::goal::start(access, node_did, &input, &create).await?;
    Ok(result)
}

#[cfg(test)]
mod tests;

pub(super) mod prepare;
